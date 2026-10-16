package ai.magicbeans.magdroid.vibedev

import ai.magicbeans.magdroid.access.MagicianAccess
import ai.magicbeans.magdroid.access.MagicianIdentity
import android.content.Context
import kotlinx.serialization.KSerializer
import kotlinx.serialization.Serializable
import java.io.File
import java.security.MessageDigest

/** The Now screen's last good read. */
@Serializable
data class VibeDevNowSnapshot(
    val runs: List<VibeDevLiveRun> = emptyList(),
    val projects: List<VibeDevProject> = emptyList(),
)

@Serializable
data class VibeDevCached<T>(val savedAtMs: Long, val value: T)

/**
 * A small on-disk copy of the last Now and run reads, so a cold start with no
 * network shows them with the usual "Updated N min ago" note instead of an
 * empty screen. One folder per machine and workspace, in the cache dir (the
 * system may clear it; nothing here is the only copy of anything).
 */
class VibeDevCache(private val directory: File) {

    fun saveNow(snapshot: VibeDevNowSnapshot, savedAtMs: Long) =
        write(NOW, VibeDevCached.serializer(VibeDevNowSnapshot.serializer()), VibeDevCached(savedAtMs, snapshot))

    fun loadNow(): VibeDevCached<VibeDevNowSnapshot>? =
        read(NOW, VibeDevCached.serializer(VibeDevNowSnapshot.serializer()))

    fun saveRun(turns: VibeDevRunTurns, savedAtMs: Long, runId: String = turns.runId) =
        write(runFile(runId), VibeDevCached.serializer(VibeDevRunTurns.serializer()), VibeDevCached(savedAtMs, turns))

    fun loadRun(runId: String): VibeDevCached<VibeDevRunTurns>? =
        read(runFile(runId), VibeDevCached.serializer(VibeDevRunTurns.serializer()))

    private fun runFile(runId: String) = "run-${digest(runId).take(24)}.json"

    private fun <T> write(name: String, serializer: KSerializer<T>, value: T) {
        runCatching {
            directory.mkdirs()
            val target = File(directory, name)
            val temporary = File(directory, "$name.tmp")
            temporary.writeText(vibeDevJson.encodeToString(serializer, value))
            if (!temporary.renameTo(target)) {
                target.delete()
                temporary.renameTo(target)
            }
            trim()
        }
    }

    private fun <T> read(name: String, serializer: KSerializer<T>): T? = runCatching {
        val file = File(directory, name)
        if (!file.isFile) null else vibeDevJson.decodeFromString(serializer, file.readText())
    }.getOrNull()

    /** Keep the newest [MAX_RUNS] run files; Now is one file. */
    private fun trim() {
        directory.listFiles { file -> file.name.startsWith("run-") && file.name.endsWith(".json") }
            ?.sortedByDescending { it.lastModified() }
            ?.drop(MAX_RUNS)
            ?.forEach { it.delete() }
    }

    companion object {
        private const val NOW = "now.json"
        const val MAX_RUNS = 20

        /**
         * The cache for the active machine and scope, or null when none is
         * set up. Callers resolve it per read, not once: a view model can
         * outlive a machine switch, and must not file one machine's runs
         * under another.
         */
        fun forConnection(context: Context): VibeDevCache? {
            val app = context.applicationContext
            val identity = MagicianAccess.activeIdentity(app) ?: return null
            return VibeDevCache(File(File(app.cacheDir, "vibedev"), key(identity)))
        }

        /** One folder per machine (by origin) and scope. */
        internal fun key(identity: MagicianIdentity): String =
            digest("${identity.origin.trim().trimEnd('/')}|${identity.principal}|${identity.workspace}").take(16)

        private fun digest(value: String): String =
            MessageDigest.getInstance("SHA-256").digest(value.toByteArray()).joinToString("") { "%02x".format(it) }
    }
}
