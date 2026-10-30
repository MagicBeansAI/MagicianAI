package ai.magicbeans.magdroid.access

import android.content.Context
import android.content.SharedPreferences
import ai.magicbeans.magdroid.mcp.MagdroidMcpServer
import android.util.Log
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey
import java.security.MessageDigest
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.asSharedFlow

/** The active machine and scope; see [MagicianAccess.activeIdentity]. */
data class MagicianIdentity(val origin: String, val principal: String, val workspace: String)

/** One atomic preference read prevents mixing credentials across a server switch. */
data class MagicianConnectionSnapshot(val baseUrl: String, val headers: Map<String, String>) {
    override fun toString(): String = "MagicianConnectionSnapshot(baseUrl=$baseUrl, credentials=<redacted>)"
}

internal data class PendingAppsEnrollmentRetry(
    val baseUrl: String,
    val enrollmentId: String,
    val enrollmentSecret: String,
    val enrollmentRequestSha256: String,
    val requestBody: String,
    val requestSha256: String,
    val connectionSecret: String,
    val connectionSecretSha256: String,
    val keyAlias: String,
    val keyId: String,
    val apkSha256: String,
    val playIntegrityCloudProjectNumber: Long,
    val automationTrustMode: AndroidAutomationTrustMode,
    val challengeBase64: String,
) {
    override fun toString(): String =
        "PendingAppsEnrollmentRetry(baseUrl=$baseUrl, enrollmentId=<redacted>, secrets=<redacted>)"
}

/**
 * Where Magician is, and how this device proves it may talk to it.
 *
 * A deliberate port of `magios/Shared/MagicianAccess.swift` rather than a new
 * design: iOS already reaches the same backend through a Cloudflare Access
 * hostname carrying a service-token pair, and a second scheme would be a second
 * thing to rotate, revoke and debug.
 *
 * Cloudflare Access proves an approved build reached the tunnel; the pairing
 * token proves which handset and owner scope. They are intentionally separate:
 * a service token is shared by app installs and cannot identify one phone.
 *
 * Secrets sit in `EncryptedSharedPreferences`, backed by the Android Keystore.
 * That is the platform's answer to the Keychain the iOS client uses; plain
 * preferences are world-readable on a rooted handset, and this is a credential
 * to the owner's entire backend.
 *
 * One profile per Magician machine. `magdroid_access_v2` holds a map of
 * origin → [MachineProfile] (base URL, Access pair, scope, device token) plus
 * `active_origin`; every accessor here reads the active profile, so callers
 * follow a machine switch without changes. `magdroid_access_v1` keeps the
 * install-wide values: the device id, the App Pilot automation enrollment
 * (tagged with the origin that issued it) and the pending-Apps retry journal
 * (which carries its own `pending_apps_base_url`). The v1 flat credential keys
 * move into v2 once, on first access.
 *
 * Switching is limited to origins already in the map. A never-paired origin
 * enters only through a completed owner-issued QR exchange ([saveEnrollment]).
 */
object MagicianAccess {

    private const val TAG = "MagicianAccess"
    private const val PREFS = "magdroid_access_v1"
    private const val PREFS_V2 = "magdroid_access_v2"

    private const val KEY_V2_PROFILES = "profiles"
    private const val KEY_V2_ACTIVE_ORIGIN = "active_origin"
    private const val KEY_V2_MIGRATED = "migrated_from_v1"

    private const val KEY_DEVICE_ID = MachineProfileBook.V1_DEVICE_ID

    /**
     * Which machine issued the App Pilot automation enrollment below. One
     * install holds at most one automation identity; it is live only while
     * that machine is the active one.
     */
    private const val KEY_AUTOMATION_ORIGIN = "apps_automation_origin"
    private const val KEY_AUTOMATION_KEY_ALIAS = "apps_automation_key_alias"
    private const val KEY_AUTOMATION_KEY_ID = "apps_automation_key_id"
    private const val KEY_AUTOMATION_APK_SHA256 = "apps_automation_apk_sha256"
    private const val KEY_AUTOMATION_ATTESTATION_POLICY_DIGEST = "apps_automation_policy_digest"
    private const val KEY_AUTOMATION_PLAY_PROJECT_NUMBER = "apps_automation_play_project_number"
    private const val KEY_AUTOMATION_TRUST_MODE = "apps_automation_trust_mode"
    private const val KEY_PENDING_APPS_BASE_URL = "pending_apps_base_url"
    private const val KEY_PENDING_APPS_ENROLLMENT_ID = "pending_apps_enrollment_id"
    private const val KEY_PENDING_APPS_ENROLLMENT_SECRET = "pending_apps_enrollment_secret"
    private const val KEY_PENDING_APPS_ENROLLMENT_REQUEST_SHA256 = "pending_apps_enrollment_request_sha256"
    private const val KEY_PENDING_APPS_REQUEST_BODY = "pending_apps_request_body"
    private const val KEY_PENDING_APPS_REQUEST_SHA256 = "pending_apps_request_sha256"
    private const val KEY_PENDING_APPS_CONNECTION_SECRET = "pending_apps_connection_secret"
    private const val KEY_PENDING_APPS_CONNECTION_SECRET_SHA256 = "pending_apps_connection_secret_sha256"
    private const val KEY_PENDING_APPS_KEY_ALIAS = "pending_apps_key_alias"
    private const val KEY_PENDING_APPS_KEY_ID = "pending_apps_key_id"
    private const val KEY_PENDING_APPS_APK_SHA256 = "pending_apps_apk_sha256"
    private const val KEY_PENDING_APPS_PLAY_PROJECT_NUMBER = "pending_apps_play_project_number"
    private const val KEY_PENDING_APPS_TRUST_MODE = "pending_apps_trust_mode"
    private const val KEY_PENDING_APPS_CHALLENGE_BASE64 = "pending_apps_challenge_base64"
    private const val MAX_PENDING_APPS_REQUEST_BYTES = 192 * 1024

    /** Compatibility defaults used only by manual recovery/legacy profiles. */
    const val PRINCIPAL = "anonymous"
    const val WORKSPACE = "default"

    const val HEADER_CLIENT_ID = "CF-Access-Client-Id"
    const val HEADER_CLIENT_SECRET = "CF-Access-Client-Secret"
    const val HEADER_AUTHORIZATION = "Authorization"
    const val HEADER_DEVICE_ID = "X-Magician-Device-Id"

    /**
     * This build's version, `versionName (versionCode)` — e.g. `0.6.13 (40)`.
     *
     * Sent wherever [HEADER_DEVICE_ID] is, so the paired machine can show
     * which build each phone runs (web Settings → Devices). Omitted when the
     * installed package reports no version name.
     */
    const val HEADER_APP_VERSION = "X-Magician-App-Version"
    private const val MAX_APP_VERSION_HEADER_CHARS = 40

    /**
     * Which dialect this build speaks on the bridge socket.
     *
     * Magician sends work down that connection, so it must know how to frame
     * the first request before making it. A build predating MCP simply omits
     * this header, which is the signal needed — probing instead would cost a
     * timeout on every older phone that connects.
     */
    const val HEADER_BRIDGE_PROTOCOL = "X-Magdroid-Protocol"

    @Volatile
    private var cached: SharedPreferences? = null
    @Volatile
    private var cachedAppVersion: String? = null
    @Volatile
    private var cachedV2: SharedPreferences? = null
    @Volatile
    private var credentialStorageUnavailable = false

    private val bookLock = Any()
    @Volatile
    private var cachedBook: MachineProfileBook? = null

    private val activeMachineChanges = MutableSharedFlow<String>(extraBufferCapacity = 8)
    private val connectionGeneration = java.util.concurrent.atomic.AtomicLong(0)
    val connectionRevision: Long get() = connectionGeneration.get()

    /**
     * The new active origin ("" for none) each time the active machine
     * changes: a switch, forgetting the active machine, signing out of it,
     * re-pairing, or changing its scope or credentials. The device bridge and the realtime hub
     * collect it to close the old machine's sockets and dial the new one.
     */
    val activeMachine: SharedFlow<String> = activeMachineChanges.asSharedFlow()

    /** Credential storage is unavailable when Android Keystore is unavailable.
     * There is no plaintext fallback: Apps pairing secrets and ordinary backend
     * credentials must never become backup-restorable SharedPreferences.
     */
    private fun prefs(context: Context): SharedPreferences {
        cached?.let { return it }
        return openEncrypted(context, PREFS).also { cached = it }
    }

    /** The per-machine profile store. Same Keystore rule as [prefs]. */
    private fun profilePrefs(context: Context): SharedPreferences {
        cachedV2?.let { return it }
        return openEncrypted(context, PREFS_V2).also { cachedV2 = it }
    }

    private fun openEncrypted(context: Context, name: String): SharedPreferences {
        if (credentialStorageUnavailable) {
            throw IllegalStateException("Android Keystore is required for Magician credentials")
        }
        return try {
            val key = MasterKey.Builder(context)
                .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
                .build()
            EncryptedSharedPreferences.create(
                context,
                name,
                key,
                EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
                EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
            )
        } catch (error: Throwable) {
            credentialStorageUnavailable = true
            Log.e(TAG, "Keystore unavailable — Magician credentials are unavailable", error)
            throw IllegalStateException("Android Keystore is required for Magician credentials", error)
        }
    }

    /**
     * The machine book, migrated from v1 on first access. Null only when the
     * Keystore is unavailable, which every reader treats as "not paired".
     */
    private fun book(context: Context): MachineProfileBook? {
        cachedBook?.let { return it }
        return synchronized(bookLock) {
            cachedBook ?: runCatching { loadOrMigrate(context) }
                .onFailure {
                    if (!credentialStorageUnavailable) Log.e(TAG, "Magician machine profiles are unavailable", it)
                }
                .getOrNull()
                ?.also { cachedBook = it }
        }
    }

    private fun loadOrMigrate(context: Context): MachineProfileBook {
        val store = profilePrefs(context)
        val stored = MachineProfileBook.decode(
            store.getString(KEY_V2_ACTIVE_ORIGIN, null),
            store.getString(KEY_V2_PROFILES, null),
        )
        val legacy = prefs(context)
        if (store.getBoolean(KEY_V2_MIGRATED, false)) {
            finishV1Migration(legacy)
            return stored
        }
        // First access on this install: a v1 install had one machine, which
        // becomes the only, active profile. A v2 map that somehow exists
        // without the marker wins over v1.
        val migrated = if (stored.isEmpty()) MachineProfileBook.migrateFromV1(legacy.all) else stored
        if (!writeBook(store, migrated)) {
            // Serve the migrated view for this process; v1 is untouched, so
            // the next process simply migrates again.
            Log.w(TAG, "Could not persist the v1 → v2 machine migration; retrying next start")
            return migrated
        }
        finishV1Migration(legacy)
        return migrated
    }

    /**
     * Move, not copy: once v2 holds the v1 credential, the flat keys go, so a
     * machine forgotten later cannot come back from v1. The App Pilot
     * enrollment, if any, is tagged with the v1 machine that issued it before
     * that machine's base URL is removed. Idempotent; re-run on each process
     * start until it succeeds.
     */
    private fun finishV1Migration(legacy: SharedPreferences) {
        val leftovers = MachineProfileBook.V1_MIGRATED_CREDENTIAL_KEYS.filter(legacy::contains)
        if (leftovers.isEmpty()) return
        val editor = legacy.edit()
        val legacyBase = legacy.getString(MachineProfileBook.V1_BASE_URL, null).orEmpty().trim()
        if (legacyBase.isNotEmpty() &&
            !legacy.getString(KEY_AUTOMATION_KEY_ALIAS, null).isNullOrBlank() &&
            legacy.getString(KEY_AUTOMATION_ORIGIN, null).isNullOrBlank()
        ) {
            editor.putString(KEY_AUTOMATION_ORIGIN, machineOriginKey(legacyBase))
        }
        leftovers.forEach(editor::remove)
        if (!editor.commit()) Log.w(TAG, "Could not remove migrated v1 credentials; retrying next start")
    }

    private fun writeBook(store: SharedPreferences, book: MachineProfileBook): Boolean {
        val editor = store.edit()
            .putString(KEY_V2_PROFILES, book.encodeProfiles())
            .putBoolean(KEY_V2_MIGRATED, true)
        book.activeOrigin?.let { editor.putString(KEY_V2_ACTIVE_ORIGIN, it) }
            ?: editor.remove(KEY_V2_ACTIVE_ORIGIN)
        return editor.commit()
    }

    /** Commit [book] and publish it to readers; throws when it cannot be stored. */
    private fun commitBook(context: Context, book: MachineProfileBook) {
        if (!writeBook(profilePrefs(context), book)) {
            throw IllegalStateException("Could not store the Magician connection")
        }
        cachedBook = book
    }

    private fun requireBook(context: Context): MachineProfileBook =
        book(context) ?: throw IllegalStateException("Android Keystore is required for Magician credentials")

    /** Apply [change] as one transaction; returns the book before and after. */
    private fun mutateBook(
        context: Context,
        change: (MachineProfileBook) -> MachineProfileBook,
    ): Pair<MachineProfileBook, MachineProfileBook> = synchronized(bookLock) {
        val before = requireBook(context)
        val after = change(before)
        if (after != before) commitBook(context, after)
        before to after
    }.also { (before, after) -> announceIfSwitched(before, after) }

    private fun announceIfSwitched(before: MachineProfileBook, after: MachineProfileBook) {
        if (after.activeConnectionChangedFrom(before)) {
            connectionGeneration.incrementAndGet()
            activeMachineChanges.tryEmit(after.activeOrigin.orEmpty())
        }
    }

    private fun active(context: Context): MachineProfile? = book(context)?.active

    /**
     * The runtime-enrolled Magician origin. There is deliberately no compiled
     * fallback: an unconfigured phone must ask for a QR instead of guessing a
     * customer endpoint and sending private data to it.
     */
    private fun readablePrefs(context: Context): SharedPreferences? =
        runCatching { prefs(context) }.getOrNull()

    private fun value(context: Context, key: String): String =
        readablePrefs(context)?.getString(key, "").orEmpty().trim()

    /** The active machine's origin. */
    fun baseUrl(context: Context): String =
        active(context)?.baseUrl.orEmpty().trim().trimEnd('/')

    /** The active machine's origin key, or null when no machine is active. */
    fun activeOrigin(context: Context): String? = book(context)?.activeOrigin

    /** Every paired machine, redacted, in pairing order. */
    fun listProfiles(context: Context): List<PairedMachine> =
        book(context)?.pairedMachines().orEmpty()

    /**
     * Use an already-paired machine. Throws [IllegalArgumentException] for an
     * origin this phone has not paired: a new machine still needs its own
     * owner-issued connection code. Listeners of [activeMachine] reconnect.
     */
    fun activate(context: Context, origin: String) {
        mutateBook(context) { it.activate(origin) }
    }

    /**
     * Remove one machine's profile and credentials. If it was active, another
     * paired machine (oldest first) becomes active, or none. An App Pilot
     * enrollment issued by that machine is discarded with it.
     */
    fun forget(context: Context, origin: String) {
        val (before, after) = mutateBook(context) { it.forget(origin) }
        val removed = before.profiles.keys - after.profiles.keys
        removed.forEach { discardAutomationIssuedBy(context, it) }
    }

    /**
     * The endpoint, for showing on Settings.
     *
     * Only the host: the scheme is noise on a settings row, and the rest of the
     * URL is fixed. Says so plainly when nothing is configured, since "blank"
     * and "default" look identical otherwise.
     */
    fun baseUrlLabel(context: Context): String {
        val raw = baseUrl(context)
        if (raw.isBlank()) return "not set"
        return raw.substringAfter("://").ifBlank { raw }
    }

    fun clientId(context: Context): String =
        active(context)?.cfAccessClientId.orEmpty().trim()

    private fun clientSecret(context: Context): String =
        active(context)?.cfAccessClientSecret.orEmpty().trim()

    /** True when a secret is stored, without handing it back. */
    fun hasClientSecret(context: Context): Boolean = clientSecret(context).isNotEmpty()

    /**
     * Complete enough to attempt a connection.
     *
     * The tunnel can run without Access during development, so an empty token
     * pair is not automatically an error — but a missing host always is, because
     * there is nowhere to dial.
     */
    fun isConfigured(context: Context): Boolean =
        baseUrl(context).isNotEmpty() && bridgeToken(context).isNotEmpty()

    /**
     * Whether this install has the hardware-bound identity required by App
     * Pilot MCP *for the active machine*. The enrollment belongs to the
     * machine that issued it; while another machine is active, App Pilot is
     * idle rather than presenting one machine's identity to another.
     */
    fun hasAutomationEnrollment(context: Context): Boolean =
        automationOrigin(context).let { it.isNotEmpty() && it == activeOrigin(context) } &&
            automationKeyAlias(context).isNotBlank() &&
            automationKeyId(context).isNotBlank() &&
            automationApkSha256(context).length == 64 &&
            automationAttestationPolicyDigest(context).startsWith("blake3:") &&
            automationTrustMode(context) != null &&
            (automationTrustMode(context) != AndroidAutomationTrustMode.PlayIntegrity ||
                automationPlayIntegrityCloudProjectNumber(context) > 0)

    fun hasAccessCredentials(context: Context): Boolean =
        clientId(context).isNotEmpty() && clientSecret(context).isNotEmpty()

    fun principal(context: Context): String =
        active(context)?.principal.orEmpty().trim().ifEmpty { PRINCIPAL }

    fun workspace(context: Context): String =
        active(context)?.workspace.orEmpty().trim().ifEmpty { WORKSPACE }

    /**
     * Which machine and scope the phone talks to now — origin, principal and
     * workspace from one profile read, so a concurrent switch cannot mix two
     * machines. Null when no machine is active. Keys per-machine local data.
     */
    fun activeIdentity(context: Context): MagicianIdentity? {
        val book = book(context) ?: return null
        val origin = book.activeOrigin?.takeIf { it.isNotBlank() } ?: return null
        val profile = book.active ?: return null
        return MagicianIdentity(
            origin = origin,
            principal = profile.principal.trim().ifEmpty { PRINCIPAL },
            workspace = profile.workspace.trim().ifEmpty { WORKSPACE },
        )
    }

    /**
     * A stable label for this install.
     *
     * Not a secret and not proof of anything — Cloudflare Access does the
     * proving. This exists so an owner with two Android phones can address one
     * of them.
     */
    fun deviceId(context: Context): String {
        val store = readablePrefs(context) ?: return ""
        store.getString(KEY_DEVICE_ID, null)?.let { return it }
        val minted = "magdroid-${java.util.UUID.randomUUID()}"
        store.edit().putString(KEY_DEVICE_ID, minted).apply()
        return minted
    }

    /**
     * Headers for any call to Magician.
     *
     * The Access pair is omitted rather than sent empty when unset, so a
     * development tunnel without Access is not handed blank credentials to
     * reject.
     */
    fun headers(context: Context): Map<String, String> {
        // One profile read, so a concurrent switch cannot mix two machines'
        // credentials in a single request.
        val profile = active(context)
        val headers = mutableMapOf(HEADER_DEVICE_ID to profileDeviceId(context, profile))
        appVersion(context)?.let { headers[HEADER_APP_VERSION] = it }
        val id = profile?.cfAccessClientId.orEmpty().trim()
        val secret = profile?.cfAccessClientSecret.orEmpty().trim()
        if (id.isNotEmpty() && secret.isNotEmpty()) {
            headers[HEADER_CLIENT_ID] = id
            headers[HEADER_CLIENT_SECRET] = secret
        }
        profile?.bridgePairingToken?.trim()?.takeIf { it.isNotEmpty() }?.let {
            // Cloudflare proves this is an allowed app; this narrower token
            // proves which paired phone and therefore which stored scope.
            headers[HEADER_AUTHORIZATION] = "Bearer $it"
        }
        return headers
    }

    /**
     * The [HEADER_APP_VERSION] value for this install, or null when the
     * package reports no version name.
     *
     * Read from PackageManager rather than BuildConfig, which this module does
     * not share with the app. Cached after the first successful read: the
     * installed version cannot change while this process lives.
     */
    fun appVersion(context: Context): String? {
        cachedAppVersion?.let { return it }
        val resolved = runCatching {
            val info = context.packageManager.getPackageInfo(context.packageName, 0)
            val code = if (android.os.Build.VERSION.SDK_INT >= android.os.Build.VERSION_CODES.P) {
                info.longVersionCode
            } else {
                @Suppress("DEPRECATION")
                info.versionCode.toLong()
            }
            formatAppVersion(info.versionName, code)
        }.getOrNull() ?: return null
        cachedAppVersion = resolved
        return resolved
    }

    /**
     * `versionName (versionCode)`, printable ASCII only and at most 40 chars,
     * or null when [versionName] is blank — a header value must not carry
     * control characters, and an empty version says nothing worth sending.
     */
    internal fun formatAppVersion(versionName: String?, versionCode: Long): String? {
        val name = versionName?.trim().orEmpty()
        if (name.isEmpty()) return null
        return "$name ($versionCode)"
            .filter { it in ' '..'~' }
            .take(MAX_APP_VERSION_HEADER_CHARS)
            .trim()
            .takeIf { it.isNotEmpty() }
    }

    /** The id this phone presented to [profile]'s machine; the install id when unrecorded. */
    private fun profileDeviceId(context: Context, profile: MachineProfile?): String =
        profile?.deviceId?.trim()?.takeIf { it.isNotEmpty() } ?: deviceId(context)

    fun connectionSnapshot(context: Context): MagicianConnectionSnapshot? =
        active(context)?.let { snapshotOf(context, it) }

    /**
     * The active machine's origin key with its connection, from one read, so
     * a request and the record of where it went cannot name two machines.
     */
    fun activeConnection(context: Context): Pair<String, MagicianConnectionSnapshot>? {
        val book = book(context) ?: return null
        val origin = book.activeOrigin?.takeIf { it.isNotBlank() } ?: return null
        val snapshot = snapshotOf(context, book.active ?: return null) ?: return null
        return origin to snapshot
    }

    /**
     * A paired machine's connection whether or not it is active, or null when
     * this phone has not paired [origin]. For cleanup the old machine still
     * owes after a switch (its push routes), never for ordinary reads.
     */
    fun connectionSnapshot(context: Context, origin: String): MagicianConnectionSnapshot? {
        val key = machineOriginKey(origin)
        if (key.isEmpty()) return null
        return snapshotOf(context, book(context)?.profiles?.get(key) ?: return null)
    }

    private fun snapshotOf(context: Context, profile: MachineProfile): MagicianConnectionSnapshot? {
        val base = profile.baseUrl.trim().trimEnd('/')
        val token = profile.bridgePairingToken.trim()
        val device = profileDeviceId(context, profile)
        if (base.isEmpty() || token.isEmpty() || device.isEmpty()) return null
        val headers = mutableMapOf(
            HEADER_DEVICE_ID to device,
            HEADER_AUTHORIZATION to "Bearer $token",
        )
        appVersion(context)?.let { headers[HEADER_APP_VERSION] = it }
        val id = profile.cfAccessClientId.trim()
        val secret = profile.cfAccessClientSecret.trim()
        if (id.isNotEmpty() && secret.isNotEmpty()) {
            headers[HEADER_CLIENT_ID] = id
            headers[HEADER_CLIENT_SECRET] = secret
        }
        return MagicianConnectionSnapshot(base, headers)
    }

    /**
     * Browser-equivalent WebSocket offers: a stable application protocol, then
     * the auth-only `magician-bearer.<token>` token. Native sockets also send
     * `Authorization`, but the voice-control upgrade selects only the
     * application protocol and still needs the bearer offer when a hop strips
     * request headers.
     */
    fun webSocketProtocols(context: Context, applicationProtocols: List<String>): List<String> {
        val application = applicationProtocols
            .map { it.trim() }
            .filter { it.isNotEmpty() && !it.startsWith("magician-bearer.") }
        val token = bridgeToken(context)
        return if (token.isEmpty()) application else application + "magician-bearer.$token"
    }

    /** The device-bridge socket URL, or null when there is no host to dial. */
    fun bridgeWebSocketUrl(context: Context): String? {
        val base = baseUrl(context)
        if (base.isEmpty()) return null
        val scheme = when {
            base.startsWith("https://") -> "wss://" + base.removePrefix("https://")
            base.startsWith("http://") -> "ws://" + base.removePrefix("http://")
            // Bare host: assume TLS. Downgrading silently would be the wrong
            // guess for a credential-bearing socket.
            else -> "wss://$base"
        }
        return "$scheme/api/magician/v2/devices/bridge"
    }

    fun save(context: Context, baseUrl: String, clientId: String, clientSecret: String) {
        // Manual recovery must not send an existing device/Access credential
        // to a different server — not even another paired machine. Only
        // verified enrollment can add an origin; only [activate] switches.
        mutateBook(context) { book ->
            requireRecoveryOrigin(book.active?.baseUrl.orEmpty(), baseUrl)
            book.updateActive { profile ->
                profile.copy(
                    cfAccessClientId = clientId.trim(),
                    // An empty secret keeps the enrolled origin's credential.
                    cfAccessClientSecret = clientSecret.trim().ifEmpty { profile.cfAccessClientSecret },
                )
            }
        }
    }

    /** The active machine's pairing token, or empty when it has none. */
    fun bridgeToken(context: Context): String =
        active(context)?.bridgePairingToken.orEmpty().trim()

    /** Recovery: replace the active machine's token. No active machine, no write. */
    fun saveBridgeToken(context: Context, token: String) {
        mutateBook(context) { book -> book.updateActive { it.copy(bridgePairingToken = token.trim()) } }
    }

    /**
     * Commit a completed QR exchange: add or replace that origin's profile and
     * make it the active machine. Other machines' profiles are kept; profiles
     * of the *same* machine under another route (same [machineName], or an
     * origin in [machineOrigins]) are removed, because the machine keeps one
     * pairing per device and this exchange just revoked their token.
     *
     * This is the only way a new origin enters the store, and it is reached
     * only after the owner-issued one-time exchange and `/devices/me` proof.
     */
    fun saveEnrollment(
        context: Context,
        baseUrl: String,
        principal: String,
        workspace: String,
        token: String,
        cloudflareClientId: String,
        cloudflareClientSecret: String,
        automationKeyAlias: String? = null,
        automationKeyId: String? = null,
        automationApkSha256: String? = null,
        automationAttestationPolicyDigest: String? = null,
        automationPlayIntegrityCloudProjectNumber: Long? = null,
        automationTrustMode: AndroidAutomationTrustMode? = null,
        pendingAppsEnrollmentId: String? = null,
        pendingAppsRequestSha256: String? = null,
        machineName: String? = null,
        machineOrigins: Collection<String> = emptyList(),
    ) {
        require(baseUrl.isNotBlank() && principal.isNotBlank() && workspace.isNotBlank() && token.isNotBlank())
        val origin = machineOriginKey(baseUrl)
        val store = prefs(context)
        val previousAlias = store.getString(KEY_AUTOMATION_KEY_ALIAS, null)
        val previousAutomationOrigin = store.getString(KEY_AUTOMATION_ORIGIN, null).orEmpty()
        var pendingToRestore: PendingAppsEnrollmentRetry? = null
        // Whether this commit replaces or drops the install's automation identity.
        var automationReplaced = false
        val editor = store.edit()
        if (automationKeyAlias != null && automationKeyId != null &&
            automationApkSha256 != null && automationAttestationPolicyDigest != null &&
            automationTrustMode != null) {
            require(
                automationKeyAlias.isNotBlank() && automationKeyId.isNotBlank() &&
                    automationApkSha256.length == 64 &&
                    automationAttestationPolicyDigest.startsWith("blake3:") &&
                    (automationTrustMode != AndroidAutomationTrustMode.PlayIntegrity ||
                        (automationPlayIntegrityCloudProjectNumber ?: 0L) > 0)
            )
            editor
                .putString(KEY_AUTOMATION_KEY_ALIAS, automationKeyAlias)
                .putString(KEY_AUTOMATION_KEY_ID, automationKeyId)
                .putString(KEY_AUTOMATION_APK_SHA256, automationApkSha256)
                .putString(KEY_AUTOMATION_ATTESTATION_POLICY_DIGEST, automationAttestationPolicyDigest)
                .putLong(KEY_AUTOMATION_PLAY_PROJECT_NUMBER, automationPlayIntegrityCloudProjectNumber ?: 0L)
                .putString(KEY_AUTOMATION_TRUST_MODE, automationTrustMode.wireValue)
                .putString(KEY_AUTOMATION_ORIGIN, origin)
            automationReplaced = true
            val pending = pendingAppsEnrollment(context)
                ?: throw IllegalStateException("The retained Apps enrollment retry is unavailable")
            require(
                pending.enrollmentId == pendingAppsEnrollmentId &&
                    pending.requestSha256 == pendingAppsRequestSha256 &&
                    pending.keyAlias == automationKeyAlias &&
                    pending.keyId == automationKeyId &&
                    pending.apkSha256 == automationApkSha256 &&
                    pending.playIntegrityCloudProjectNumber == (automationPlayIntegrityCloudProjectNumber ?: 0L) &&
                    pending.automationTrustMode == automationTrustMode &&
                    pending.connectionSecret == token,
            )
            pendingToRestore = pending
            removePendingAppsEnrollment(editor)
        } else {
            require(pendingAppsEnrollment(context) == null) {
                "An Android Apps enrollment is awaiting exact recovery"
            }
            // A plain mobile code from the machine that issued App Pilot (or
            // an untagged legacy enrollment) replaces that grant, as before.
            // Pairing a *different* machine leaves the issuing machine's
            // App Pilot enrollment in place for when it is active again.
            if (previousAutomationOrigin.isEmpty() || previousAutomationOrigin == origin) {
                removeAutomationEnrollment(editor)
                automationReplaced = true
            }
        }
        val profile = MachineProfile(
            baseUrl = origin,
            cfAccessClientId = cloudflareClientId.trim(),
            cfAccessClientSecret = cloudflareClientSecret.trim(),
            principal = principal.trim(),
            workspace = workspace.trim(),
            bridgePairingToken = token.trim(),
            deviceId = deviceId(context),
            machineName = normalizedMachineName(machineName),
        )
        // The profile commits first; if the install-wide half then fails, the
        // profile is rolled back so this process never observes a credential
        // whose Apps journal is gone.
        val (before, after) = synchronized(bookLock) {
            val before = requireBook(context)
            val after = before.upsertReplacingSameMachine(profile, machineOrigins)
            commitBook(context, after)
            if (!editor.commit()) {
                // The server/Desktop authority may already be committed and the
                // retained journal still references this exact key. Destroying it
                // here would make a response-loss retry impossible. Only the
                // proven-unsubmitted/explicit-Gone path may discard this alias.
                pendingToRestore?.let { pending ->
                    // SharedPreferences updates its in-process map before the disk
                    // write reports success. Restore the exact retry into that map
                    // (and retry its disk write) so this process cannot observe a
                    // half-installed credential with its recovery journal removed.
                    putPendingAppsEnrollment(store.edit(), pending).commit()
                }
                // Roll the profile back: the previous connection is kept.
                runCatching { commitBook(context, before) }
                throw IllegalStateException("Could not durably store the Apps enrollment")
            }
            before to after
        }
        if (automationReplaced && !previousAlias.isNullOrBlank() && previousAlias != automationKeyAlias) {
            AndroidAutomationIdentityManager(context.applicationContext).discard(previousAlias)
        }
        // Same-machine profiles this exchange replaced: their token is dead,
        // and an App Pilot grant that route issued goes with it, as on forget.
        (before.profiles.keys - after.profiles.keys).forEach { discardAutomationIssuedBy(context, it) }
        announceIfSwitched(before, after)
    }

    /** The origin whose machine issued the App Pilot enrollment, or empty. */
    internal fun automationOrigin(context: Context): String =
        value(context, KEY_AUTOMATION_ORIGIN)

    private fun removeAutomationEnrollment(editor: SharedPreferences.Editor): SharedPreferences.Editor = editor
        .remove(KEY_AUTOMATION_KEY_ALIAS)
        .remove(KEY_AUTOMATION_KEY_ID)
        .remove(KEY_AUTOMATION_APK_SHA256)
        .remove(KEY_AUTOMATION_ATTESTATION_POLICY_DIGEST)
        .remove(KEY_AUTOMATION_PLAY_PROJECT_NUMBER)
        .remove(KEY_AUTOMATION_TRUST_MODE)
        .remove(KEY_AUTOMATION_ORIGIN)

    /**
     * Drop the App Pilot enrollment when [origin]'s machine issued it (or it
     * is an untagged legacy one), and destroy its Keystore key. The pending
     * Apps retry journal is left alone: only an exact retry or a proven-unspent
     * response may discard it.
     */
    private fun discardAutomationIssuedBy(context: Context, origin: String) {
        val store = prefs(context)
        val alias = store.getString(KEY_AUTOMATION_KEY_ALIAS, null)
        val issuer = store.getString(KEY_AUTOMATION_ORIGIN, null).orEmpty()
        if (alias.isNullOrBlank() || (issuer.isNotEmpty() && issuer != origin)) return
        removeAutomationEnrollment(store.edit()).apply()
        AndroidAutomationIdentityManager(context.applicationContext).discard(alias)
    }

    internal fun automationKeyAlias(context: Context): String =
        value(context, KEY_AUTOMATION_KEY_ALIAS)

    internal fun automationKeyId(context: Context): String =
        value(context, KEY_AUTOMATION_KEY_ID)

    internal fun automationApkSha256(context: Context): String =
        value(context, KEY_AUTOMATION_APK_SHA256)

    internal fun automationAttestationPolicyDigest(context: Context): String =
        value(context, KEY_AUTOMATION_ATTESTATION_POLICY_DIGEST)

    internal fun automationPlayIntegrityCloudProjectNumber(context: Context): Long =
        readablePrefs(context)?.getLong(KEY_AUTOMATION_PLAY_PROJECT_NUMBER, 0L) ?: 0L

    internal fun automationTrustMode(context: Context): AndroidAutomationTrustMode? =
        AndroidAutomationTrustMode.fromWire(value(context, KEY_AUTOMATION_TRUST_MODE))
            ?: AndroidAutomationTrustMode.PlayIntegrity.takeIf {
                automationPlayIntegrityCloudProjectNumber(context) > 0
            }

    internal fun pendingAppsEnrollment(context: Context): PendingAppsEnrollmentRetry? {
        val store = prefs(context)
        val enrollmentId = store.getString(KEY_PENDING_APPS_ENROLLMENT_ID, null) ?: return null
        val value = PendingAppsEnrollmentRetry(
            baseUrl = store.getString(KEY_PENDING_APPS_BASE_URL, "").orEmpty(),
            enrollmentId = enrollmentId,
            enrollmentSecret = store.getString(KEY_PENDING_APPS_ENROLLMENT_SECRET, "").orEmpty(),
            enrollmentRequestSha256 = store.getString(KEY_PENDING_APPS_ENROLLMENT_REQUEST_SHA256, "").orEmpty(),
            requestBody = store.getString(KEY_PENDING_APPS_REQUEST_BODY, "").orEmpty(),
            requestSha256 = store.getString(KEY_PENDING_APPS_REQUEST_SHA256, "").orEmpty(),
            connectionSecret = store.getString(KEY_PENDING_APPS_CONNECTION_SECRET, "").orEmpty(),
            connectionSecretSha256 = store.getString(KEY_PENDING_APPS_CONNECTION_SECRET_SHA256, "").orEmpty(),
            keyAlias = store.getString(KEY_PENDING_APPS_KEY_ALIAS, "").orEmpty(),
            keyId = store.getString(KEY_PENDING_APPS_KEY_ID, "").orEmpty(),
            apkSha256 = store.getString(KEY_PENDING_APPS_APK_SHA256, "").orEmpty(),
            playIntegrityCloudProjectNumber = store.getLong(KEY_PENDING_APPS_PLAY_PROJECT_NUMBER, 0L),
            automationTrustMode = AndroidAutomationTrustMode.fromWire(
                store.getString(KEY_PENDING_APPS_TRUST_MODE, "").orEmpty(),
            ) ?: AndroidAutomationTrustMode.PlayIntegrity.takeIf {
                store.getLong(KEY_PENDING_APPS_PLAY_PROJECT_NUMBER, 0L) > 0
            } ?: throw IllegalStateException("The retained Apps enrollment trust mode is corrupt"),
            challengeBase64 = store.getString(KEY_PENDING_APPS_CHALLENGE_BASE64, "").orEmpty(),
        )
        if (!validPendingAppsEnrollment(value)) {
            throw IllegalStateException("The retained Apps enrollment retry is corrupt")
        }
        return value
    }

    internal fun retainPendingAppsEnrollment(
        context: Context,
        value: PendingAppsEnrollmentRetry,
    ) {
        require(validPendingAppsEnrollment(value))
        pendingAppsEnrollment(context)?.let { existing ->
            require(existing == value) { "A different Apps enrollment retry is already retained" }
            return
        }
        val committed = putPendingAppsEnrollment(prefs(context).edit(), value).commit()
        if (!committed) throw IllegalStateException("Could not retain the Apps enrollment retry")
    }

    internal fun rotatePendingAppsIntegrityToken(
        context: Context,
        enrollmentId: String,
        expectedRequestSha256: String,
        requestBody: String,
    ): String {
        require(requestBody.toByteArray(Charsets.UTF_8).size <= MAX_PENDING_APPS_REQUEST_BYTES)
        val pending = pendingAppsEnrollment(context)
            ?: throw IllegalStateException("The retained Apps enrollment retry is unavailable")
        require(pending.enrollmentId == enrollmentId && pending.requestSha256 == expectedRequestSha256)
        val nextDigest = sha256Hex(requestBody)
        if (!prefs(context).edit()
                .putString(KEY_PENDING_APPS_REQUEST_BODY, requestBody)
                .putString(KEY_PENDING_APPS_REQUEST_SHA256, nextDigest)
                .commit()) {
            throw IllegalStateException("Could not rotate the retained Play Integrity token")
        }
        return nextDigest
    }

    internal fun abandonPendingAppsEnrollment(
        context: Context,
        enrollmentId: String,
        requestSha256: String,
    ): String? {
        val pending = pendingAppsEnrollment(context) ?: return null
        require(pending.enrollmentId == enrollmentId && pending.requestSha256 == requestSha256)
        if (!removePendingAppsEnrollment(prefs(context).edit()).commit()) {
            throw IllegalStateException("Could not clear the Apps enrollment retry")
        }
        return pending.keyAlias
    }

    private fun validPendingAppsEnrollment(value: PendingAppsEnrollmentRetry): Boolean =
        value.baseUrl.length in 1..2048 && value.enrollmentId.length in 16..128 &&
            value.enrollmentSecret.length in 32..256 && value.requestBody.isNotEmpty() &&
            value.requestBody.toByteArray(Charsets.UTF_8).size <= MAX_PENDING_APPS_REQUEST_BYTES &&
            value.enrollmentRequestSha256.isLowerHexDigest() && value.requestSha256.isLowerHexDigest() &&
            sha256Hex(value.requestBody) == value.requestSha256 &&
            value.connectionSecret.length in 32..128 && value.connectionSecretSha256.isLowerHexDigest() &&
            sha256Hex(value.connectionSecret) == value.connectionSecretSha256 &&
            value.keyAlias.length in 1..256 && value.keyId.length in 1..256 &&
            value.apkSha256.isLowerHexDigest() &&
            (value.automationTrustMode != AndroidAutomationTrustMode.PlayIntegrity ||
                value.playIntegrityCloudProjectNumber > 0)
            && runCatching { android.util.Base64.decode(value.challengeBase64, android.util.Base64.DEFAULT) }
                .getOrNull()?.size == 32

    private fun Char.isHexDigit(): Boolean = this in '0'..'9' || this in 'a'..'f'

    private fun String.isLowerHexDigest(): Boolean = length == 64 && all { it.isHexDigit() }

    private fun sha256Hex(value: String): String = MessageDigest.getInstance("SHA-256")
        .digest(value.toByteArray(Charsets.UTF_8))
        .joinToString("") { byte -> "%02x".format(byte) }

    private fun removePendingAppsEnrollment(editor: SharedPreferences.Editor): SharedPreferences.Editor = editor
        .remove(KEY_PENDING_APPS_BASE_URL)
        .remove(KEY_PENDING_APPS_ENROLLMENT_ID)
        .remove(KEY_PENDING_APPS_ENROLLMENT_SECRET)
        .remove(KEY_PENDING_APPS_ENROLLMENT_REQUEST_SHA256)
        .remove(KEY_PENDING_APPS_REQUEST_BODY)
        .remove(KEY_PENDING_APPS_REQUEST_SHA256)
        .remove(KEY_PENDING_APPS_CONNECTION_SECRET)
        .remove(KEY_PENDING_APPS_CONNECTION_SECRET_SHA256)
        .remove(KEY_PENDING_APPS_KEY_ALIAS)
        .remove(KEY_PENDING_APPS_KEY_ID)
        .remove(KEY_PENDING_APPS_APK_SHA256)
        .remove(KEY_PENDING_APPS_PLAY_PROJECT_NUMBER)
        .remove(KEY_PENDING_APPS_TRUST_MODE)
        .remove(KEY_PENDING_APPS_CHALLENGE_BASE64)

    private fun putPendingAppsEnrollment(
        editor: SharedPreferences.Editor,
        value: PendingAppsEnrollmentRetry,
    ): SharedPreferences.Editor = editor
        .putString(KEY_PENDING_APPS_BASE_URL, value.baseUrl)
        .putString(KEY_PENDING_APPS_ENROLLMENT_ID, value.enrollmentId)
        .putString(KEY_PENDING_APPS_ENROLLMENT_SECRET, value.enrollmentSecret)
        .putString(KEY_PENDING_APPS_ENROLLMENT_REQUEST_SHA256, value.enrollmentRequestSha256)
        .putString(KEY_PENDING_APPS_REQUEST_BODY, value.requestBody)
        .putString(KEY_PENDING_APPS_REQUEST_SHA256, value.requestSha256)
        .putString(KEY_PENDING_APPS_CONNECTION_SECRET, value.connectionSecret)
        .putString(KEY_PENDING_APPS_CONNECTION_SECRET_SHA256, value.connectionSecretSha256)
        .putString(KEY_PENDING_APPS_KEY_ALIAS, value.keyAlias)
        .putString(KEY_PENDING_APPS_KEY_ID, value.keyId)
        .putString(KEY_PENDING_APPS_APK_SHA256, value.apkSha256)
        .putLong(KEY_PENDING_APPS_PLAY_PROJECT_NUMBER, value.playIntegrityCloudProjectNumber)
        .putString(KEY_PENDING_APPS_TRUST_MODE, value.automationTrustMode.wireValue)
        .putString(KEY_PENDING_APPS_CHALLENGE_BASE64, value.challengeBase64)

    /** Access-only headers used before the phone has a paired device identity. */
    fun accessHeaders(context: Context): Map<String, String> {
        val headers = mutableMapOf<String, String>()
        val id = clientId(context)
        val secret = clientSecret(context)
        if (id.isNotEmpty() && secret.isNotEmpty()) {
            headers[HEADER_CLIENT_ID] = id
            headers[HEADER_CLIENT_SECRET] = secret
        }
        return headers
    }

    /**
     * Headers for the device-bridge socket specifically. [headers] already
     * carries the device credential so every Android API request retains the
     * scope established by enrollment; this adds only the MCP dialect.
     */
    fun bridgeHeaders(context: Context): Map<String, String> {
        val headers = headers(context).toMutableMap()
        headers[HEADER_BRIDGE_PROTOCOL] = "mcp/" + MagdroidMcpServer.PROTOCOL_VERSION
        return headers
    }

    /**
     * Sign out of the active machine only.
     *
     * Its profile — base URL, Access pair, scope and pairing token — is
     * removed, along with the App Pilot enrollment if that machine issued it.
     * Other paired machines keep their own credentials, and none of them
     * becomes active: signing out never silently starts talking to another
     * machine. The pairing token never moves between machines, so no origin
     * can inherit another's credential. The device id survives: signing out
     * is not a new install, and keeping it means reconnecting the same
     * handset does not orphan its identity.
     */
    fun clear(context: Context) {
        val (before, _) = mutateBook(context) { it.clearActive() }
        before.activeOrigin?.let { discardAutomationIssuedBy(context, it) }
    }
}
