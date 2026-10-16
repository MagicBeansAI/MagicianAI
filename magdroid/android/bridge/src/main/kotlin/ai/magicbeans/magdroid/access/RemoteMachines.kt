package ai.magicbeans.magdroid.access

import android.content.Context
import io.ktor.client.HttpClient
import io.ktor.client.engine.cio.CIO
import io.ktor.client.plugins.HttpTimeout
import io.ktor.client.request.get
import io.ktor.client.request.header
import io.ktor.client.statement.bodyAsText
import io.ktor.http.isSuccess
import java.io.IOException
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/** One entry of the owner's machine registry, as the active machine lists it. */
@Serializable
data class RemoteMachine(
    val name: String,
    @SerialName("api_origin") val apiOrigin: String,
    @SerialName("ui_origin") val uiOrigin: String,
    @SerialName("is_self") val isSelf: Boolean = false,
)

/** `GET /api/magician/v2/remote-machines`. No secrets: names and public origins only. */
@Serializable
data class RemoteMachineRegistry(
    val self: String? = null,
    val machines: List<RemoteMachine> = emptyList(),
)

private val registryJson = Json { ignoreUnknownKeys = true }

/**
 * Parse the registry, keeping only entries with a usable origin pair. The
 * phone opens `ui_origin` in the browser, so an origin that fails the same
 * rules as a pairing QR (https, or http to a private address; no credentials,
 * path, query or fragment) is dropped rather than offered. Origins are
 * normalized so they compare equal to stored profile keys.
 */
internal fun parseRemoteMachineRegistry(body: String): RemoteMachineRegistry? {
    val decoded = runCatching {
        registryJson.decodeFromString(RemoteMachineRegistry.serializer(), body)
    }.getOrNull() ?: return null
    val machines = decoded.machines.mapNotNull { machine ->
        val api = DeviceEnrollmentLinks.normalizedHttpOrigin(machine.apiOrigin) ?: return@mapNotNull null
        val ui = DeviceEnrollmentLinks.normalizedHttpOrigin(machine.uiOrigin) ?: return@mapNotNull null
        val name = machine.name.trim().takeIf { it.isNotEmpty() } ?: return@mapNotNull null
        machine.copy(name = name, apiOrigin = api, uiOrigin = ui)
    }
    return RemoteMachineRegistry(decoded.self?.trim()?.takeIf { it.isNotEmpty() }, machines)
}

/** One row of the Settings machine picker. */
data class MachinePickerRow(
    /** Registry name when the registry knows this origin, else the host. */
    val title: String,
    /** The API origin; for a paired row, the key [MagicianAccess.activate] takes. */
    val origin: String,
    /** Where to open this machine's web UI (pair from its Settings → Mobile devices). */
    val uiOrigin: String?,
    val paired: Boolean,
    val active: Boolean,
)

/**
 * Paired machines first, in pairing order, then registry machines this phone
 * has not paired. Only paired rows can be switched to; an unpaired row only
 * points at the machine's own web UI, where its owner-issued QR is minted.
 *
 * A paired row is titled by the name the machine gave at pairing, else its
 * registry name, else its host. A registry entry whose name matches a paired
 * machine's stored name is that machine by another route, so it is not
 * offered again as unpaired.
 */
fun machinePickerRows(
    paired: List<PairedMachine>,
    registry: RemoteMachineRegistry?,
): List<MachinePickerRow> {
    val registered = registry?.machines.orEmpty()
    val byOrigin = registered.associateBy { it.apiOrigin }
    fun byName(name: String?): RemoteMachine? =
        normalizedMachineName(name)?.let { wanted -> registered.firstOrNull { it.name.equals(wanted, ignoreCase = true) } }
    val pairedRows = paired.map { machine ->
        val known = byOrigin[machine.origin] ?: byName(machine.machineName)
        MachinePickerRow(
            title = normalizedMachineName(machine.machineName) ?: known?.name ?: machine.hostLabel,
            origin = machine.origin,
            uiOrigin = known?.uiOrigin,
            paired = true,
            active = machine.active,
        )
    }
    val pairedOrigins = paired.map { it.origin }.toSet()
    val pairedNames = paired.mapNotNull { normalizedMachineName(it.machineName)?.lowercase() }.toSet()
    val unpairedRows = registered
        .filter { it.apiOrigin !in pairedOrigins && it.name.lowercase() !in pairedNames }
        .map { MachinePickerRow(it.name, it.apiOrigin, it.uiOrigin, paired = false, active = false) }
    return pairedRows + unpairedRows
}

/**
 * Reads the owner's machine registry from the active machine, with that
 * machine's own Access pair and device token (the same headers as every other
 * request). Each machine's credential stays with that machine.
 */
class RemoteMachinesClient internal constructor(
    private val client: HttpClient,
    private val snapshot: () -> MagicianConnectionSnapshot?,
) {
    constructor(context: Context) : this(
        HttpClient(CIO) {
            install(HttpTimeout) {
                connectTimeoutMillis = 10_000
                requestTimeoutMillis = 15_000
            }
        },
        { MagicianAccess.connectionSnapshot(context.applicationContext) },
    )

    /**
     * The registry, or an empty one when the active machine predates the
     * endpoint (404). Throws [IOException] for anything else unusable.
     */
    suspend fun fetch(): RemoteMachineRegistry {
        val connection = snapshot() ?: throw IOException("No Magician machine is active.")
        val response = client.get("${connection.baseUrl}$PATH") {
            connection.headers.forEach { (name, value) -> header(name, value) }
        }
        if (response.status.value == 404) return RemoteMachineRegistry()
        if (!response.status.isSuccess()) {
            throw IOException("Magician could not list machines (HTTP ${response.status.value}).")
        }
        return parseRemoteMachineRegistry(response.bodyAsText())
            ?: throw IOException("Magician returned an unreadable machine list.")
    }

    fun close() = client.close()

    companion object {
        const val PATH = "/api/magician/v2/remote-machines"
    }
}
