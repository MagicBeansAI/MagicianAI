//
//  CanonicalMapLocalPrefs.swift
//  Magios
//
//  S2d — the small client-local preference store for canonical-backed thinking
//  maps. The canonical server owns the durable map graph (title, lifecycle,
//  nodes, edges), but three affordances are DELIBERATELY client-local and have
//  no server metadata:
//
//    - pin           (which maps float to the top of the library)
//    - preferredMode (Canvas / Focus / Outline lens the user last used)
//    - lastOpened    (recency ordering within the library)
//
//  It ALSO tracks which E0 `UserDefaults` maps have already been imported into
//  the canonical backend (S2e), so the one-time importer is idempotent.
//
//  This type is PURE + UNGATED (no `#if`): it is just a namespaced
//  `UserDefaults` wrapper with no dependency on the gated backend, so it compiles
//  and unit-tests in the normal (flag-OFF) build. It uses its OWN namespaced key
//  (`thinking-map.canonical.localprefs`) and NEVER touches E0's
//  `thinking-map.library.v2` key.
//

import Foundation

/// A single map's client-local preferences (everything the canonical server
/// does not model). All fields default to the E0 defaults.
struct CanonicalMapLocalPref: Codable, Equatable {
    var isPinned: Bool = false
    var preferredMode: ThinkingMapMode = .map
    /// RFC3339 timestamp of the last local open, or nil if never opened locally.
    var lastOpenedAt: Date?

    init(isPinned: Bool = false, preferredMode: ThinkingMapMode = .map, lastOpenedAt: Date? = nil) {
        self.isPinned = isPinned
        self.preferredMode = preferredMode
        self.lastOpenedAt = lastOpenedAt
    }
}

/// The persisted envelope: a `[mapID: pref]` table plus the set of E0 local map
/// ids already imported into the canonical backend (the S2e idempotency ledger).
private struct CanonicalMapLocalPrefsEnvelope: Codable, Equatable {
    var prefs: [String: CanonicalMapLocalPref] = [:]
    var importedLocalMapIDs: [String] = []
}

/// `UserDefaults`-backed store for the client-local canonical-map preferences.
/// Namespaced under its OWN key so it never collides with E0's library data.
final class CanonicalMapLocalPrefs {
    /// The namespaced key. Deliberately distinct from
    /// `ThinkingMapModel.persistenceKey` (`thinking-map.library.v2`).
    static let storageKey = "thinking-map.canonical.localprefs"

    private let defaults: UserDefaults?
    private let key: String
    private var envelope: CanonicalMapLocalPrefsEnvelope

    init(defaults: UserDefaults? = .standard, key: String = CanonicalMapLocalPrefs.storageKey) {
        self.defaults = defaults
        self.key = key
        if let defaults,
           let data = defaults.data(forKey: key),
           let decoded = try? JSONDecoder().decode(CanonicalMapLocalPrefsEnvelope.self, from: data) {
            envelope = decoded
        } else {
            envelope = CanonicalMapLocalPrefsEnvelope()
        }
    }

    // MARK: Per-map preferences

    /// The stored preferences for `mapID`, or the defaults if none were saved.
    func pref(for mapID: String) -> CanonicalMapLocalPref {
        envelope.prefs[mapID] ?? CanonicalMapLocalPref()
    }

    func isPinned(_ mapID: String) -> Bool { pref(for: mapID).isPinned }
    func preferredMode(_ mapID: String) -> ThinkingMapMode { pref(for: mapID).preferredMode }
    func lastOpenedAt(_ mapID: String) -> Date? { pref(for: mapID).lastOpenedAt }

    /// Toggle the pin flag for `mapID` and persist. Returns the new value.
    @discardableResult
    func togglePinned(_ mapID: String) -> Bool {
        var p = pref(for: mapID)
        p.isPinned.toggle()
        setPref(p, for: mapID)
        return p.isPinned
    }

    /// Set the preferred lens for `mapID` and persist.
    func setPreferredMode(_ mode: ThinkingMapMode, for mapID: String) {
        var p = pref(for: mapID)
        p.preferredMode = mode
        setPref(p, for: mapID)
    }

    /// Stamp `mapID` as opened `at` (defaults to now) and persist.
    func markOpened(_ mapID: String, at date: Date = Date()) {
        var p = pref(for: mapID)
        p.lastOpenedAt = date
        setPref(p, for: mapID)
    }

    /// Drop all local prefs for `mapID` (e.g. on a soft-delete). Idempotent.
    func forget(_ mapID: String) {
        guard envelope.prefs[mapID] != nil else { return }
        envelope.prefs[mapID] = nil
        persist()
    }

    // MARK: Import ledger (S2e idempotency)

    /// True when the E0 local map id has already been imported into canonical.
    func isImported(localMapID: String) -> Bool {
        envelope.importedLocalMapIDs.contains(localMapID)
    }

    /// Record that the E0 local map id has been imported. Idempotent.
    func markImported(localMapID: String) {
        guard !envelope.importedLocalMapIDs.contains(localMapID) else { return }
        envelope.importedLocalMapIDs.append(localMapID)
        persist()
    }

    /// The full set of imported local ids (mainly for tests/diagnostics).
    var importedLocalMapIDs: [String] { envelope.importedLocalMapIDs }

    // MARK: Internals

    private func setPref(_ pref: CanonicalMapLocalPref, for mapID: String) {
        envelope.prefs[mapID] = pref
        persist()
    }

    private func persist() {
        guard let defaults, let data = try? JSONEncoder().encode(envelope) else { return }
        defaults.set(data, forKey: key)
    }
}
