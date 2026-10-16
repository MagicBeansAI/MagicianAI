package ai.magicbeans.magdroid.access

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.MapSerializer
import kotlinx.serialization.builtins.serializer
import kotlinx.serialization.json.Json

/**
 * One paired Magician machine: its origin and the credentials that machine
 * issued to this phone.
 *
 * Nothing here is shared with another machine. Each profile's device token
 * and Cloudflare Access pair came from that machine's own owner-issued QR, so
 * a profile can only ever be used against its own [baseUrl].
 */
@Serializable
internal data class MachineProfile(
    @SerialName("base_url") val baseUrl: String,
    @SerialName("cf_access_client_id") val cfAccessClientId: String = "",
    @SerialName("cf_access_client_secret") val cfAccessClientSecret: String = "",
    val principal: String = "",
    val workspace: String = "",
    /**
     * The device token this machine minted in its one-time enrollment
     * exchange. Distinct from the Access pair, which is shared by enrolled
     * app installs and says only "an allowed native client"; this one says
     * *which handset*, so it never leaves the machine that issued it.
     */
    @SerialName("bridge_pairing_token") val bridgePairingToken: String = "",
    @SerialName("device_id") val deviceId: String = "",
    /**
     * The machine's own name from its enrollment exchange (`machine_name`),
     * or null for a machine that predates it and for profiles stored before
     * it existed. Two profiles with the same name are the same machine
     * reached by two routes (same-Wi-Fi vs remote).
     */
    @SerialName("machine_name") val machineName: String? = null,
) {
    /** The `scheme://host[:port]` this profile is stored under. */
    val origin: String get() = machineOriginKey(baseUrl)

    /** The host alone, for a label when the registry has no name for it. */
    val hostLabel: String get() = baseUrl.substringAfter("://").ifBlank { baseUrl }

    /** Whether [other] names the same machine: both carry a non-blank, equal name. */
    fun sameMachineNameAs(other: String?): Boolean {
        val mine = normalizedMachineName(machineName) ?: return false
        val theirs = normalizedMachineName(other) ?: return false
        return mine.equals(theirs, ignoreCase = true)
    }

    override fun toString(): String =
        "MachineProfile(baseUrl=$baseUrl, principal=$principal, workspace=$workspace, credentials=<redacted>)"
}

/**
 * A paired machine as Settings may show it: no credential leaves the store.
 * [machineName] is the name the machine gave at pairing, when it gave one.
 */
data class PairedMachine(
    val origin: String,
    val hostLabel: String,
    val active: Boolean,
    val machineName: String? = null,
)

/** A machine name as stored and compared: trimmed, or null when blank. */
internal fun normalizedMachineName(raw: String?): String? = raw?.trim()?.takeIf { it.isNotEmpty() }

/** Stable map key for an origin; unparseable legacy values keep their trimmed form. */
internal fun machineOriginKey(raw: String): String =
    DeviceEnrollmentLinks.normalizedHttpOrigin(raw) ?: raw.trim().trimEnd('/')

/**
 * Every machine this phone has paired with, plus the one it is using.
 *
 * Immutable: each operation returns the next book, which [MagicianAccess]
 * commits as one preference transaction. That keeps the rules — especially
 * "a never-paired origin cannot be activated" — testable without Android.
 */
internal data class MachineProfileBook(
    val activeOrigin: String? = null,
    val profiles: Map<String, MachineProfile> = emptyMap(),
) {
    /** The profile every accessor reads, or null when no machine is active. */
    val active: MachineProfile?
        get() = activeOrigin?.let(profiles::get)

    /** A re-pair or credential rotation at the same origin changes authority
     * just as a machine switch does. A display-name edit does not. */
    fun activeConnectionChangedFrom(before: MachineProfileBook): Boolean =
        activeOrigin != before.activeOrigin ||
            active?.copy(machineName = null) != before.active?.copy(machineName = null)

    fun isEmpty(): Boolean = profiles.isEmpty()

    /** The redacted list Settings shows, in pairing order. */
    fun pairedMachines(): List<PairedMachine> = profiles.map { (origin, profile) ->
        PairedMachine(
            origin = origin,
            hostLabel = profile.hostLabel,
            active = origin == activeOrigin,
            machineName = normalizedMachineName(profile.machineName),
        )
    }

    /**
     * Add or replace [profile] and make it active. Only a completed
     * owner-issued exchange (or the one-time v1 migration) may call this; it
     * is the single way a new origin enters the book.
     */
    fun upsert(profile: MachineProfile): MachineProfileBook {
        val key = profile.origin
        require(key.isNotEmpty()) { "A machine profile needs an origin" }
        val stored = profile.copy(baseUrl = key)
        return copy(activeOrigin = key, profiles = profiles + (key to stored))
    }

    /**
     * Commit a completed enrollment that the machine says replaced its older
     * pairing with this phone.
     *
     * The server keeps one pairing per device per machine, so pairing a
     * machine by its other route (same-Wi-Fi `http://192.168.x.x:3002` vs its
     * remote `https://` origin) revokes the token the older profile holds.
     * Every *other* stored profile that is the same machine — the same
     * non-null machine name, or an origin listed in [machineOrigins] (the
     * exchange's `machine_origins`, normalized as profiles are keyed) — is
     * removed, then [profile] is added and made active as [upsert] does.
     *
     * This only removes profiles; the one origin it adds is the one just
     * exchanged, so a never-paired origin still needs its own owner-issued
     * code.
     */
    fun upsertReplacingSameMachine(
        profile: MachineProfile,
        machineOrigins: Collection<String> = emptyList(),
    ): MachineProfileBook {
        val key = profile.origin
        val origins = machineOrigins.map(::machineOriginKey).filter(String::isNotEmpty).toSet()
        val stale = profiles.filter { (origin, stored) ->
            origin != key && (origin in origins || stored.sameMachineNameAs(profile.machineName))
        }.keys
        return copy(profiles = profiles - stale).upsert(profile)
    }

    /**
     * Switch to an already-paired machine.
     *
     * Throws [IllegalArgumentException] for an origin with no stored profile:
     * switching is not pairing, and a never-paired origin still needs its own
     * owner-issued connection code.
     */
    fun activate(origin: String): MachineProfileBook =
        copy(activeOrigin = requirePairedOrigin(profiles.keys, origin))

    /**
     * Remove one machine's credentials. Forgetting the active machine falls
     * back to another stored machine (oldest first), or to none.
     */
    fun forget(origin: String): MachineProfileBook {
        val key = pairedKey(origin) ?: return this
        val remaining = profiles - key
        val nextActive = if (activeOrigin == key) remaining.keys.firstOrNull() else activeOrigin
        return MachineProfileBook(nextActive, remaining)
    }

    /**
     * Sign out of the active machine: its credentials go, the others stay,
     * and nothing becomes active. Unlike [forget], this never silently starts
     * talking to a different machine.
     */
    fun clearActive(): MachineProfileBook {
        val key = activeOrigin ?: return this
        return MachineProfileBook(null, profiles - key)
    }

    /** Replace the active profile's fields; a no-op when nothing is active. */
    fun updateActive(transform: (MachineProfile) -> MachineProfile): MachineProfileBook {
        val current = active ?: return this
        val key = requireNotNull(activeOrigin)
        return copy(profiles = profiles + (key to transform(current).copy(baseUrl = key)))
    }

    private fun pairedKey(origin: String): String? {
        val wanted = machineOriginKey(origin)
        return profiles.keys.firstOrNull { it == wanted }
    }

    fun encodeProfiles(): String = json.encodeToString(profileMapSerializer, profiles)

    override fun toString(): String =
        "MachineProfileBook(activeOrigin=$activeOrigin, origins=${profiles.keys})"

    companion object {
        private val json = Json { ignoreUnknownKeys = true; encodeDefaults = true }
        private val profileMapSerializer = MapSerializer(String.serializer(), MachineProfile.serializer())

        /**
         * Decode the stored map. A corrupt map decodes as empty rather than
         * throwing on every request; the owner re-pairs, which is the same
         * recovery a lost Keystore needs.
         */
        fun decode(activeOrigin: String?, rawProfiles: String?): MachineProfileBook {
            val profiles = rawProfiles?.takeIf(String::isNotBlank)?.let {
                runCatching { json.decodeFromString(profileMapSerializer, it) }.getOrNull()
            }.orEmpty()
            val active = activeOrigin?.trim()?.takeIf { it.isNotEmpty() && it in profiles }
            return MachineProfileBook(active, profiles)
        }

        /**
         * Build the first book from the `magdroid_access_v1` flat keys.
         *
         * A v1 install had exactly one machine, so it becomes the only,
         * active profile. No base URL means nothing was paired: an empty book.
         */
        fun migrateFromV1(v1: Map<String, *>): MachineProfileBook {
            fun string(key: String) = (v1[key] as? String).orEmpty().trim()
            val base = string(V1_BASE_URL).trimEnd('/')
            if (base.isEmpty()) return MachineProfileBook()
            return MachineProfileBook().upsert(
                MachineProfile(
                    baseUrl = base,
                    cfAccessClientId = string(V1_CLIENT_ID),
                    cfAccessClientSecret = string(V1_CLIENT_SECRET),
                    principal = string(V1_PRINCIPAL),
                    workspace = string(V1_WORKSPACE),
                    bridgePairingToken = string(V1_BRIDGE_TOKEN),
                    deviceId = string(V1_DEVICE_ID),
                ),
            )
        }

        // The v1 flat keys this migration reads. `device_id` stays in v1
        // after migration: it is the install's identity, not a credential.
        internal const val V1_BASE_URL = "base_url"
        internal const val V1_CLIENT_ID = "cf_access_client_id"
        internal const val V1_CLIENT_SECRET = "cf_access_client_secret"
        internal const val V1_DEVICE_ID = "device_id"
        internal const val V1_PRINCIPAL = "principal"
        internal const val V1_WORKSPACE = "workspace"
        internal const val V1_BRIDGE_TOKEN = "bridge_pairing_token"

        /** Credential keys moved out of v1 once the v2 book holds them. */
        internal val V1_MIGRATED_CREDENTIAL_KEYS = listOf(
            V1_BASE_URL, V1_CLIENT_ID, V1_CLIENT_SECRET, V1_PRINCIPAL, V1_WORKSPACE, V1_BRIDGE_TOKEN,
        )
    }
}
