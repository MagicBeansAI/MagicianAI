import Foundation

/// The client-side source of truth for which tutor primitives exist and how each
/// renders. It loads the bundled recipe set synchronously at first access (so
/// `isSupported` stays synchronous), and `refresh(session:)` fetches the merged
/// scoped set from the backend, caches it to `Caches/`, and falls back to the
/// disk cache then the bundled set on failure.
///
/// A new primitive is a new recipe file served by the backend — no app rebuild.
final class TutorPrimitiveRegistry {
    static let shared = TutorPrimitiveRegistry()

    private let endpoint = URL(
        string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2/tutor/primitives"
    )!
    private let cacheFileName = "tutor_primitives.json"
    private let lock = NSLock()

    /// Whether this instance reads/writes the shared on-disk cache. The `shared`
    /// singleton does; test-injected instances don't, so a test refresh can never
    /// poison the shared bundled set.
    private let persistsCache: Bool

    private var byType: [String: TutorRecipe] = [:]

    // MARK: - Init

    private init() {
        persistsCache = true
        let recipes = Self.loadBundledRecipes()
        index(recipes)
    }

    /// Test seam: build a registry from an explicit recipe set (no bundle, no
    /// network, no shared disk cache).
    init(recipes: [TutorRecipe]) {
        persistsCache = false
        index(recipes)
    }

    // MARK: - Lookup (synchronous)

    /// The recipe for a shape `type`, matching the primary `type` or any alias.
    func recipe(for type: String) -> TutorRecipe? {
        lock.lock(); defer { lock.unlock() }
        return byType[type.lowercased()]
    }

    /// Whether any loaded recipe renders this `type`.
    func isSupported(_ type: String) -> Bool {
        recipe(for: type) != nil
    }

    /// All loaded recipes (dedup'd by primary type).
    var recipes: [TutorRecipe] {
        lock.lock(); defer { lock.unlock() }
        var seen = Set<String>()
        var out: [TutorRecipe] = []
        for recipe in byType.values where !seen.contains(recipe.type.lowercased()) {
            seen.insert(recipe.type.lowercased())
            out.append(recipe)
        }
        return out
    }

    // MARK: - Refresh (async)

    /// Fetch the merged scoped recipe set from the backend and adopt it. On any
    /// failure the current in-memory set is **kept** (never downgraded); the disk
    /// cache is only consulted as a source when the current set is empty (e.g. an
    /// empty bundle on first launch before the initial fetch).
    @discardableResult
    func refresh(session: URLSession = .shared) async -> Bool {
        var request = URLRequest(url: endpoint)
        request.httpMethod = "GET"
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        MagicianAccess.authorize(&request)

        do {
            let (data, response) = try await session.data(for: request)
            guard let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode),
                  let recipes = Self.decode(data), !recipes.isEmpty else {
                loadDiskCacheIfEmpty()
                return false
            }
            index(recipes)
            writeDiskCache(data)
            return true
        } catch {
            loadDiskCacheIfEmpty()
            return false
        }
    }

    // MARK: - Indexing

    private func index(_ recipes: [TutorRecipe]) {
        lock.lock(); defer { lock.unlock() }
        var map: [String: TutorRecipe] = [:]
        for recipe in recipes.prefix(512) {   // hostile-input cap
            for key in recipe.matchedTypes { map[key] = recipe }
        }
        byType = map
    }

    // MARK: - Disk cache

    private var cacheURL: URL? {
        guard persistsCache else { return nil }
        return FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask).first?
            .appendingPathComponent(cacheFileName)
    }

    private var isEmpty: Bool {
        lock.lock(); defer { lock.unlock() }
        return byType.isEmpty
    }

    private func writeDiskCache(_ data: Data) {
        guard let url = cacheURL else { return }
        try? data.write(to: url, options: .atomic)
    }

    /// Adopt the disk cache only when the current set is empty — a failed refresh
    /// must never downgrade a good in-memory (bundled/previous) set. The empty
    /// re-check and the write happen under ONE lock acquisition so a concurrent
    /// successful refresh can't be clobbered between a released check and the write.
    private func loadDiskCacheIfEmpty() {
        guard let url = cacheURL, let data = try? Data(contentsOf: url),
              let recipes = Self.decode(data), !recipes.isEmpty else { return }
        lock.lock(); defer { lock.unlock() }
        guard byType.isEmpty else { return }            // re-check under the write lock
        var map: [String: TutorRecipe] = [:]
        for recipe in recipes.prefix(512) {
            for key in recipe.matchedTypes { map[key] = recipe }
        }
        byType = map
    }

    // MARK: - Decoding (accepts a bare array or `{ "primitives": [...] }`)

    static func decode(_ data: Data) -> [TutorRecipe]? {
        let decoder = JSONDecoder()
        if let array = try? decoder.decode([TutorRecipe].self, from: data) {
            return array
        }
        if let wrapper = try? decoder.decode(PrimitivesEnvelope.self, from: data) {
            return wrapper.primitives
        }
        return nil
    }

    private struct PrimitivesEnvelope: Decodable {
        let primitives: [TutorRecipe]
    }

    // MARK: - Bundled set

    private static func loadBundledRecipes() -> [TutorRecipe] {
        // Search the main bundle and the bundle hosting this class (covers the
        // hosted-unit-test case where `Bundle.main` may resolve differently).
        let candidates = [Bundle.main, Bundle(for: TutorPrimitiveRegistry.self)]
        for bundle in candidates {
            if let url = bundle.url(forResource: "tutor_primitives", withExtension: "json"),
               let data = try? Data(contentsOf: url),
               let recipes = decode(data), !recipes.isEmpty {
                return recipes
            }
        }
        return []
    }
}
