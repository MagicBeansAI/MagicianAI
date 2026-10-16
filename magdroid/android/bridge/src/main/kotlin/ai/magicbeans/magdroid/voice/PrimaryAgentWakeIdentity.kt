package ai.magicbeans.magdroid.voice

import ai.magicbeans.magdroid.access.MagicianAccess
import android.content.Context
import io.ktor.client.HttpClient
import io.ktor.client.engine.cio.CIO
import io.ktor.client.plugins.HttpTimeout
import io.ktor.client.request.get
import io.ktor.client.request.header
import io.ktor.client.request.parameter
import io.ktor.client.statement.bodyAsText
import io.ktor.http.isSuccess
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import java.text.Normalizer

private val wakeIdentityJson = Json {
    ignoreUnknownKeys = true
    explicitNulls = false
    coerceInputValues = true
}

/** The scoped primary-agent identity from Magician's canonical Crew API. */
@Serializable
data class PrimaryAgentWakeIdentity(
    @SerialName("agent_id") val agentId: String,
    val name: String,
    val aliases: List<String> = emptyList(),
    /**
     * In-lexicon spellings the on-device spotter arms INSTEAD of [name], for
     * names the wake model has no vocabulary entry for. The recogniser is
     * grammar-constrained and silently drops unknown words, so such a name can
     * never fire however it is pronounced. Empty means [name] is armable as
     * spelled. Display and open-vocabulary paths never use this.
     *
     * Defaulted so an identity cached before this field still decodes.
     */
    @SerialName("wake_spellings") val wakeSpellings: List<String> = emptyList(),
)

/**
 * Cached primary-agent identity and its local wake phrases.
 *
 * The wake service needs its grammar before a voice session exists, so it
 * cannot use `session.ready` addressing. This mirrors iOS: refresh the scoped
 * `/v2/agents` primary record while the app is foregrounded, retain the last
 * known identity for offline starts, and derive the same `Hey <name>` phrases
 * the backend derives from canonical name plus aliases.
 */
object PrimaryAgentWakeIdentityStore {
    private const val PREFS = "magdroid_primary_agent_identity_v1"
    private const val KEY_IDENTITY = "identity"
    private val refreshMutex = Mutex()
    private val client = HttpClient(CIO) {
        install(HttpTimeout) {
            connectTimeoutMillis = 10_000
            requestTimeoutMillis = 20_000
            socketTimeoutMillis = 20_000
        }
    }

    @Volatile private var initialized = false
    private val _identity = MutableStateFlow<PrimaryAgentWakeIdentity?>(null)
    val identity: StateFlow<PrimaryAgentWakeIdentity?> = _identity.asStateFlow()
    private val _phrases = MutableStateFlow<List<String>>(emptyList())
    val phrases: StateFlow<List<String>> = _phrases.asStateFlow()
    private val _problem = MutableStateFlow<String?>(null)
    val problem: StateFlow<String?> = _problem.asStateFlow()

    fun initialize(context: Context) {
        if (initialized) return
        synchronized(this) {
            if (initialized) return
            val encoded = context.applicationContext
                .getSharedPreferences(PREFS, Context.MODE_PRIVATE)
                .getString(KEY_IDENTITY, null)
            val cached = encoded?.let {
                runCatching {
                    wakeIdentityJson.decodeFromString(PrimaryAgentWakeIdentity.serializer(), it)
                }.getOrNull()
            }
            publish(cached)
            initialized = true
        }
    }

    /** Refresh once from the authority; a failure retains the last good cache. */
    suspend fun refresh(context: Context): Boolean = refreshMutex.withLock {
        initialize(context)
        val result = runCatching {
            val root = MagicianAccess.baseUrl(context).trimEnd('/')
            require(root.isNotEmpty()) { "No Magician host is configured." }
            val response = client.get("$root/api/magician/v2/agents") {
                MagicianAccess.headers(context).forEach { (name, value) -> header(name, value) }
                parameter("offset", 0)
                parameter("limit", 500)
            }
            require(response.status.isSuccess()) { "Magician returned ${response.status.value}." }
            decodePrimaryAgent(response.bodyAsText())
        }
        val refreshed = result.getOrNull()
        if (refreshed == null) {
            if (_identity.value == null) {
                _problem.value = "Magican couldn't load the primary assistant name. Check the Magician connection and try again."
            }
            return@withLock false
        }
        _problem.value = null
        if (_identity.value == refreshed) return@withLock false
        context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .edit()
            .putString(
                KEY_IDENTITY,
                wakeIdentityJson.encodeToString(PrimaryAgentWakeIdentity.serializer(), refreshed),
            )
            .apply()
        publish(refreshed)
        true
    }

    private fun publish(value: PrimaryAgentWakeIdentity?) {
        _identity.value = value
        _phrases.value = activationPhrases(value)
    }
}

@Serializable
private data class AgentListEnvelope(val agents: List<AgentRecord> = emptyList())

@Serializable
private data class AgentRecord(val definition: AgentDefinition)

@Serializable
private data class AgentDefinition(
    @SerialName("agent_id") val agentId: String = "",
    val name: String = "",
    val aliases: List<String> = emptyList(),
    @SerialName("wake_spellings") val wakeSpellings: List<String> = emptyList(),
    @SerialName("is_primary") val isPrimary: Boolean = false,
)

internal fun decodePrimaryAgent(body: String): PrimaryAgentWakeIdentity {
    val envelope = wakeIdentityJson.decodeFromString(AgentListEnvelope.serializer(), body)
    val primary = envelope.agents.firstOrNull { it.definition.isPrimary }?.definition
        ?: error("The scoped agent catalog has no primary assistant.")
    val id = primary.agentId.trim()
    val name = primary.name.trim()
    require(id.isNotEmpty() && name.isNotEmpty()) { "The primary assistant identity is incomplete." }
    return PrimaryAgentWakeIdentity(id, name, primary.aliases, primary.wakeSpellings)
}

/** Exact Android port of iOS `AmbientActivationPhrases.forArming`. */
internal fun activationPhrases(identity: PrimaryAgentWakeIdentity?): List<String> {
    if (identity == null) return emptyList()
    val seen = mutableSetOf<String>()
    // A spelling override replaces the advertised names because the shipped
    // identity's canonical name cannot be armed by this recogniser.
    val spellings = identity.wakeSpellings.map(String::trim).filter { it.isNotEmpty() }
    val armable = if (spellings.isEmpty()) listOf(identity.name) + identity.aliases else spellings
    return armable.mapNotNull { advertised ->
        val words = Regex("[\\p{L}\\p{N}]+").findAll(advertised).map { it.value }.toMutableList()
        if (words.firstOrNull().equals("hey", ignoreCase = true)) words.removeAt(0)
        if (words.isEmpty()) return@mapNotNull null
        val name = words.joinToString(" ")
        if (!seen.add(name.foldForIdentity())) return@mapNotNull null
        "Hey $name"
    }
}

/** Android equivalent of iOS's case- and diacritic-insensitive identity fold. */
private fun String.foldForIdentity(): String = Normalizer.normalize(this, Normalizer.Form.NFD)
    .replace(Regex("\\p{M}+"), "")
    .lowercase()
