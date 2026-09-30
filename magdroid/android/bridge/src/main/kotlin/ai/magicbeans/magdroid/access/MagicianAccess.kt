package ai.magicbeans.magdroid.access

import android.content.Context
import android.content.SharedPreferences
import ai.magicbeans.magdroid.mcp.MagdroidMcpServer
import android.util.Log
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey
import java.security.MessageDigest

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
 */
object MagicianAccess {

    private const val TAG = "MagicianAccess"
    private const val PREFS = "magdroid_access_v1"

    private const val KEY_BASE_URL = "base_url"
    private const val KEY_CLIENT_ID = "cf_access_client_id"
    private const val KEY_CLIENT_SECRET = "cf_access_client_secret"
    private const val KEY_DEVICE_ID = "device_id"
    private const val KEY_PRINCIPAL = "principal"
    private const val KEY_WORKSPACE = "workspace"

    /**
     * The device token minted by the one-time enrollment exchange.
     *
     * Distinct from the Access credentials above, and it has to be: those are
     * shared by enrolled app installs and say only "an allowed native client".
     * This one says *which handset*, and the bridge grants whoever holds it the
     * taps and screenshots meant for this phone.
     */
    private const val KEY_BRIDGE_TOKEN = "bridge_pairing_token"
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
    private var credentialStorageUnavailable = false

    /** Credential storage is unavailable when Android Keystore is unavailable.
     * There is no plaintext fallback: Apps pairing secrets and ordinary backend
     * credentials must never become backup-restorable SharedPreferences.
     */
    private fun prefs(context: Context): SharedPreferences {
        cached?.let { return it }
        if (credentialStorageUnavailable) {
            throw IllegalStateException("Android Keystore is required for Magician credentials")
        }
        val store = try {
            val key = MasterKey.Builder(context)
                .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
                .build()
            EncryptedSharedPreferences.create(
                context,
                PREFS,
                key,
                EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
                EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
            )
        } catch (error: Throwable) {
            credentialStorageUnavailable = true
            Log.e(TAG, "Keystore unavailable — Magician credentials are unavailable", error)
            throw IllegalStateException("Android Keystore is required for Magician credentials", error)
        }
        cached = store
        return store
    }

    /**
     * The runtime-enrolled Magician origin. There is deliberately no compiled
     * fallback: an unconfigured phone must ask for a QR instead of guessing a
     * customer endpoint and sending private data to it.
     */
    private fun readablePrefs(context: Context): SharedPreferences? =
        runCatching { prefs(context) }.getOrNull()

    private fun value(context: Context, key: String): String =
        readablePrefs(context)?.getString(key, "").orEmpty().trim()

    fun baseUrl(context: Context): String =
        value(context, KEY_BASE_URL).trimEnd('/')

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
        value(context, KEY_CLIENT_ID)

    private fun clientSecret(context: Context): String =
        value(context, KEY_CLIENT_SECRET)

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

    /** Whether this install has the hardware-bound identity required by App Pilot MCP. */
    fun hasAutomationEnrollment(context: Context): Boolean =
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
        readablePrefs(context)?.getString(KEY_PRINCIPAL, PRINCIPAL).orEmpty().trim().ifEmpty { PRINCIPAL }

    fun workspace(context: Context): String =
        readablePrefs(context)?.getString(KEY_WORKSPACE, WORKSPACE).orEmpty().trim().ifEmpty { WORKSPACE }

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
        val headers = mutableMapOf(HEADER_DEVICE_ID to deviceId(context))
        val id = clientId(context)
        val secret = clientSecret(context)
        if (id.isNotEmpty() && secret.isNotEmpty()) {
            headers[HEADER_CLIENT_ID] = id
            headers[HEADER_CLIENT_SECRET] = secret
        }
        bridgeToken(context).takeIf { it.isNotEmpty() }?.let {
            // Cloudflare proves this is an allowed app; this narrower token
            // proves which paired phone and therefore which stored scope.
            headers[HEADER_AUTHORIZATION] = "Bearer $it"
        }
        return headers
    }

    fun connectionSnapshot(context: Context): MagicianConnectionSnapshot? {
        val values = readablePrefs(context)?.all ?: return null
        fun string(key: String) = (values[key] as? String).orEmpty().trim()
        val base = string(KEY_BASE_URL).trimEnd('/')
        val token = string(KEY_BRIDGE_TOKEN)
        val device = string(KEY_DEVICE_ID)
        if (base.isEmpty() || token.isEmpty() || device.isEmpty()) return null
        val headers = mutableMapOf(
            HEADER_DEVICE_ID to device,
            HEADER_AUTHORIZATION to "Bearer $token",
        )
        val id = string(KEY_CLIENT_ID)
        val secret = string(KEY_CLIENT_SECRET)
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
        // to a different server. Only verified enrollment can replace origin.
        val enrolledOrigin = requireRecoveryOrigin(baseUrl(context), baseUrl)
        val editor = prefs(context).edit()
            .putString(KEY_BASE_URL, enrolledOrigin)
            .putString(KEY_CLIENT_ID, clientId.trim())
        // An empty secret keeps the enrolled origin's credential.
        if (clientSecret.isNotBlank()) {
            editor.putString(KEY_CLIENT_SECRET, clientSecret.trim())
        }
        editor.apply()
    }

    /** The pairing token, or empty when this handset has not been enrolled. */
    fun bridgeToken(context: Context): String =
        readablePrefs(context)?.getString(KEY_BRIDGE_TOKEN, "").orEmpty().trim()

    fun saveBridgeToken(context: Context, token: String) {
        prefs(context).edit().putString(KEY_BRIDGE_TOKEN, token.trim()).apply()
    }

    /** Commit a completed QR exchange as one preference transaction. */
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
    ) {
        require(baseUrl.isNotBlank() && principal.isNotBlank() && workspace.isNotBlank() && token.isNotBlank())
        val store = prefs(context)
        val previousAlias = store.getString(KEY_AUTOMATION_KEY_ALIAS, null)
        var pendingToRestore: PendingAppsEnrollmentRetry? = null
        val editor = store.edit()
            .putString(KEY_BASE_URL, baseUrl.trim().trimEnd('/'))
            .putString(KEY_PRINCIPAL, principal.trim())
            .putString(KEY_WORKSPACE, workspace.trim())
            .putString(KEY_BRIDGE_TOKEN, token.trim())
            .putString(KEY_CLIENT_ID, cloudflareClientId.trim())
            .putString(KEY_CLIENT_SECRET, cloudflareClientSecret.trim())
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
            editor.remove(KEY_AUTOMATION_KEY_ALIAS)
                .remove(KEY_AUTOMATION_KEY_ID)
                .remove(KEY_AUTOMATION_APK_SHA256)
                .remove(KEY_AUTOMATION_ATTESTATION_POLICY_DIGEST)
                .remove(KEY_AUTOMATION_PLAY_PROJECT_NUMBER)
                .remove(KEY_AUTOMATION_TRUST_MODE)
        }
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
            throw IllegalStateException("Could not durably store the Apps enrollment")
        }
        if (!previousAlias.isNullOrBlank() && previousAlias != automationKeyAlias) {
            AndroidAutomationIdentityManager(context.applicationContext).discard(previousAlias)
        }
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

    fun clear(context: Context) {
        // The device id survives: signing out is not a new install, and keeping
        // it means reconnecting the same handset does not orphan its identity.
        val store = prefs(context)
        val automationAlias = store.getString(KEY_AUTOMATION_KEY_ALIAS, null)
        store.edit()
            .remove(KEY_BASE_URL)
            .remove(KEY_CLIENT_ID)
            .remove(KEY_CLIENT_SECRET)
            // The pairing token goes too: it authorises this handset against a
            // particular Magician, and pointing at another one must not carry
            // the old credential along.
            .remove(KEY_BRIDGE_TOKEN)
            .remove(KEY_PRINCIPAL)
            .remove(KEY_WORKSPACE)
            .remove(KEY_AUTOMATION_KEY_ALIAS)
            .remove(KEY_AUTOMATION_KEY_ID)
            .remove(KEY_AUTOMATION_APK_SHA256)
            .remove(KEY_AUTOMATION_ATTESTATION_POLICY_DIGEST)
            .apply()
        if (!automationAlias.isNullOrBlank()) {
            AndroidAutomationIdentityManager(context.applicationContext).discard(automationAlias)
        }
    }
}
