import CryptoKit
import Foundation

/// The last Now / Projects data and each run's turns, as JSON in the Caches
/// directory, so a cold start without a network shows the last data under
/// "Updated N min ago". One folder per paired machine and, inside it, per
/// principal and workspace, so neither a machine switch nor a re-pair as
/// someone else shows another account's runs. Forgetting a machine removes
/// its folder. Best effort: a missing or unreadable file is just no cache.
struct VibeDevCache {
    /// What Now and Projects show.
    struct NowSnapshot: Codable, Equatable {
        let live: VibeDevLiveRunsResponse
        let projects: [VibeDevProject]
    }

    private struct Envelope<Value: Codable>: Codable {
        let savedAt: Date
        let value: Value
        enum CodingKeys: String, CodingKey {
            case savedAt = "saved_at"
            case value
        }
    }

    let directory: URL

    init(directory: URL) {
        self.directory = directory
    }

    /// The cache for the active machine and account, or nil when none is paired.
    static func forActiveMachine(fileManager: FileManager = .default) -> VibeDevCache? {
        let paired = MagicianAccess.connectionProfiles
        guard let origin = paired.activeOrigin, let profile = paired.active,
              let root = Self.root(fileManager: fileManager) else { return nil }
        return VibeDevCache(root: root, origin: origin, principal: profile.principal, workspace: profile.workspace)
    }

    /// `<root>/<origin>/<principal>/<workspace>`.
    init(root: URL, origin: String, principal: String, workspace: String) {
        self.init(directory: Self.machineFolder(root: root, origin: origin)
            .appendingPathComponent(Self.folderName(principal), isDirectory: true)
            .appendingPathComponent(Self.folderName(workspace), isDirectory: true))
    }

    /// Drop every account's cache for a machine the phone forgot.
    static func forget(origin: String, root: URL? = nil, fileManager: FileManager = .default) {
        guard let root = root ?? Self.root(fileManager: fileManager) else { return }
        try? fileManager.removeItem(at: machineFolder(root: root, origin: origin))
    }

    private static func root(fileManager: FileManager) -> URL? {
        fileManager.urls(for: .cachesDirectory, in: .userDomainMask).first?
            .appendingPathComponent("VibeDev", isDirectory: true)
    }

    /// Keyed like `MagicianAccess.activeOrigin` (normalized, lowercased), so
    /// a forget by any spelling of the origin finds the folder.
    private static func machineFolder(root: URL, origin: String) -> URL {
        let key = MobileConnectionProfiles.originKey(origin) ?? origin
        return root.appendingPathComponent(folderName(key), isDirectory: true)
    }

    /// A file-system-safe name for an origin or id, distinct for distinct
    /// input, even on a case-insensitive file system. Lowercase ASCII letters,
    /// digits, `-` and `.` stay; every other UTF-8 byte (`_` and capitals
    /// included) becomes `_xx`. A bare "." or ".." has its dots escaped
    /// too, "" is "_", and a name past 120 characters is cut to 100 plus `~`
    /// and a SHA-256 prefix of the input (neither `_` alone nor `~` is ever
    /// produced otherwise).
    static func folderName(_ raw: String) -> String {
        guard !raw.isEmpty else { return "_" }
        let escapeDots = raw == "." || raw == ".."
        var name = ""
        for byte in raw.utf8 {
            switch byte {
            case UInt8(ascii: "a")...UInt8(ascii: "z"), UInt8(ascii: "0")...UInt8(ascii: "9"),
                 UInt8(ascii: "-"):
                name.append(Character(UnicodeScalar(byte)))
            case UInt8(ascii: ".") where !escapeDots:
                name.append(".")
            default:
                name += String(format: "_%02x", byte)
            }
        }
        guard name.count > 120 else { return name }
        let digest = SHA256.hash(data: Data(raw.utf8)).prefix(8).map { String(format: "%02x", $0) }.joined()
        return String(name.prefix(100)) + "~" + digest
    }

    func saveNow(_ snapshot: NowSnapshot, at date: Date = Date()) {
        save(snapshot, key: "now", at: date)
    }

    func loadNow() -> (NowSnapshot, Date)? { load(NowSnapshot.self, key: "now") }

    func saveTurns(_ turns: VibeDevRunTurns, for runID: String, at date: Date = Date()) {
        save(turns, key: "turns-" + Self.folderName(runID), at: date)
    }

    func loadTurns(for runID: String) -> (VibeDevRunTurns, Date)? {
        load(VibeDevRunTurns.self, key: "turns-" + Self.folderName(runID))
    }

    func clear() {
        try? FileManager.default.removeItem(at: directory)
    }

    private func url(_ key: String) -> URL { directory.appendingPathComponent(key + ".json") }

    private func save<Value: Codable>(_ value: Value, key: String, at date: Date) {
        do {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            let encoder = JSONEncoder()
            encoder.dateEncodingStrategy = .millisecondsSince1970
            let data = try encoder.encode(Envelope(savedAt: date, value: value))
            try data.write(to: url(key), options: [.atomic])
        } catch {
            debugLog("VibeDev cache write failed for \(key)")
        }
    }

    private func load<Value: Codable>(_ type: Value.Type, key: String) -> (Value, Date)? {
        guard let data = try? Data(contentsOf: url(key)) else { return nil }
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .millisecondsSince1970
        guard let envelope = try? decoder.decode(Envelope<Value>.self, from: data) else { return nil }
        return (envelope.value, envelope.savedAt)
    }
}
