package ai.magicbeans.magdroid.tutor

import ai.magicbeans.magdroid.access.MagicianAccess
import android.content.Context
import android.util.Log
import io.ktor.client.HttpClient
import io.ktor.client.engine.cio.CIO
import io.ktor.client.plugins.HttpTimeout
import io.ktor.client.request.get
import io.ktor.client.request.header
import io.ktor.client.request.parameter
import io.ktor.client.statement.bodyAsText
import io.ktor.http.isSuccess
import java.io.File
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * Which tutor primitives exist, and how each one is drawn.
 *
 * The client-side source of truth for the recipe path. Lookup is by shape type
 * or by any alias a recipe answers to, and nothing here knows the name of a
 * single primitive — that is the point. A scope authors a JSON file under its
 * `tutor_primitives/` folder and this registry serves it to the renderer, so a
 * new shape draws without shipping a build.
 *
 * Deliberately free of [Context] so it can be exercised as a plain unit: the
 * Android side of the job — assets, disk cache, the fetch — is
 * [TutorPrimitiveSource].
 */
class TutorPrimitiveRegistry(recipes: List<TutorRecipe> = emptyList()) {

    @Volatile
    private var index: Map<String, TutorRecipe> = buildIndex(recipes)

    /** Replace the whole set. Later recipes win, as the backend merges them. */
    fun replaceAll(recipes: List<TutorRecipe>) {
        index = buildIndex(recipes)
    }

    /** The recipe for a shape type, under its own name or any alias. */
    fun recipe(type: String): TutorRecipe? = index[type.trim().lowercase()]

    /** Whether the recipe path can draw this type at all. */
    fun isSupported(type: String): Boolean = recipe(type) != null

    /** Every type and alias currently served. */
    val knownTypes: Set<String> get() = index.keys

    val isEmpty: Boolean get() = index.isEmpty()

    /**
     * Index by type and alias.
     *
     * A later recipe overwrites an earlier one on collision, matching the
     * backend's own merge — it reads the global folder first and a scope's
     * second, precisely so a scope can redefine a built-in.
     */
    private fun buildIndex(recipes: List<TutorRecipe>): Map<String, TutorRecipe> {
        val out = mutableMapOf<String, TutorRecipe>()
        recipes.take(TutorRecipe.MAX_RECIPES).forEach { recipe ->
            recipe.matchedTypes.forEach { type -> out[type] = recipe }
        }
        return out
    }

    companion object {
        /** The set the app draws from. */
        val shared = TutorPrimitiveRegistry()
    }
}

/**
 * Where a registry's recipes come from on a handset.
 *
 * Three sources, in order of authority: the backend, then the last set it
 * served (cached on disk), then the recipes bundled with the build. The bundle
 * is what makes the very first launch — before any fetch, or offline — draw the
 * same primitives as everything else, and it is copied from the canonical
 * server folder at build time rather than hand-maintained, so a primitive added
 * to the backend reaches this app on the next build instead of when somebody
 * remembers.
 */
object TutorPrimitiveSource {

    private const val TAG = "TutorPrimitives"
    private const val ASSET_DIR = "tutor_primitives"
    private const val CACHE_FILE = "tutor_primitives.json"
    private const val PATH = "api/magician/v2/tutor/primitives"

    /** A recipe set is small; this bounds a hostile or broken response. */
    private const val MAX_RESPONSE_BYTES = 2 * 1024 * 1024

    private val client by lazy {
        HttpClient(CIO) {
            install(HttpTimeout) {
                requestTimeoutMillis = 15_000
                connectTimeoutMillis = 10_000
            }
        }
    }

    /**
     * Fill [registry] from the best source available without going to network.
     *
     * Called on startup so the canvas can draw immediately; [refresh] then
     * replaces the set once the backend answers.
     */
    fun loadLocal(context: Context, registry: TutorPrimitiveRegistry = TutorPrimitiveRegistry.shared) {
        val cached = readCache(context)
        if (!cached.isNullOrEmpty()) {
            registry.replaceAll(cached)
            return
        }
        val bundled = readBundled(context)
        if (bundled.isNotEmpty()) registry.replaceAll(bundled)
    }

    /**
     * Fetch the scope's primitives and replace the set.
     *
     * Returns whether the registry now holds a backend-served set. A failure
     * leaves whatever was already loaded in place — an offline handset keeps
     * drawing the primitives it has rather than losing the canvas.
     */
    suspend fun refresh(
        context: Context,
        registry: TutorPrimitiveRegistry = TutorPrimitiveRegistry.shared,
    ): Boolean = withContext(Dispatchers.IO) {
        val host = MagicianAccess.baseUrl(context).trimEnd('/')
        if (host.isBlank()) return@withContext false
        try {
            val response = client.get("$host/$PATH") {
                MagicianAccess.headers(context).forEach { (name, value) -> header(name, value) }
            }
            if (!response.status.isSuccess()) return@withContext false
            val body = response.bodyAsText()
            if (body.length > MAX_RESPONSE_BYTES) {
                Log.w(TAG, "primitives response too large (${body.length} bytes); keeping current set")
                return@withContext false
            }
            val recipes = TutorRecipe.decodeList(body)
            if (recipes.isNullOrEmpty()) return@withContext false

            registry.replaceAll(recipes)
            writeCache(context, body)
            Log.i(TAG, "loaded ${recipes.size} tutor primitives")
            true
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (error: Exception) {
            Log.w(TAG, "primitive refresh failed; keeping the current set: ${error.message}")
            false
        }
    }

    /** The recipes copied into assets at build time from the canonical folder. */
    private fun readBundled(context: Context): List<TutorRecipe> = try {
        val names = context.assets.list(ASSET_DIR).orEmpty().filter { it.endsWith(".json") }
        names.mapNotNull { name ->
            runCatching {
                context.assets.open("$ASSET_DIR/$name").bufferedReader().use { it.readText() }
            }.getOrNull()?.let { raw ->
                // Each asset is one recipe; a bad file costs itself, not the set.
                TutorRecipe.decodeList("[$raw]")?.firstOrNull()
            }
        }
    } catch (error: Exception) {
        Log.w(TAG, "no bundled primitives: ${error.message}")
        emptyList()
    }

    private fun cacheFile(context: Context) = File(context.filesDir, CACHE_FILE)

    private fun readCache(context: Context): List<TutorRecipe>? = runCatching {
        val file = cacheFile(context)
        if (!file.exists() || file.length() > MAX_RESPONSE_BYTES) return@runCatching null
        TutorRecipe.decodeList(file.readText())
    }.getOrNull()

    private fun writeCache(context: Context, body: String) {
        runCatching {
            val target = cacheFile(context)
            // Write beside and rename, so a kill mid-write cannot leave a
            // truncated file that then fails to parse on every later launch.
            val temporary = File(target.parentFile, "$CACHE_FILE.tmp")
            temporary.writeText(body)
            if (!temporary.renameTo(target)) {
                temporary.copyTo(target, overwrite = true)
                temporary.delete()
            }
        }.onFailure { Log.w(TAG, "could not cache primitives: ${it.message}") }
    }
}
