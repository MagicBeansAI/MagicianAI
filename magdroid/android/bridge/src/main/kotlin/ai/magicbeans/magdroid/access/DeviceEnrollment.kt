package ai.magicbeans.magdroid.access

import android.content.Context
import android.os.Build
import android.util.Base64
import io.ktor.client.HttpClient
import io.ktor.client.engine.cio.CIO
import io.ktor.client.plugins.HttpTimeout
import io.ktor.client.request.get
import io.ktor.client.request.header
import io.ktor.client.request.post
import io.ktor.client.request.setBody
import io.ktor.client.statement.bodyAsText
import io.ktor.http.ContentType
import io.ktor.http.contentType
import io.ktor.http.isSuccess
import java.net.InetAddress
import java.net.URI
import java.net.URLDecoder
import java.security.MessageDigest
import java.security.SecureRandom
import kotlin.io.encoding.Base64 as KotlinBase64
import kotlin.io.encoding.ExperimentalEncodingApi
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/** Inbound custom-scheme contract. Magican links use `magican://` exclusively. */
object MagicanAppLinks {
    val schemes: Set<String> = setOf("magican")
    fun isScheme(scheme: String?): Boolean = scheme in schemes
}

/** A validated, short-lived capability decoded from Magician's pairing QR. */
data class DeviceEnrollmentLink(
    val baseUrl: String,
    val enrollmentId: String,
    val secret: String,
    val clientKind: String = "android",
    val appsAutomation: Boolean = false,
    val challenge: ByteArray? = null,
    val playIntegrityCloudProjectNumber: Long? = null,
    val automationTrustMode: AndroidAutomationTrustMode? = null,
) {
    val connectionMode: DeviceConnectionMode
        get() = DeviceConnectionMode.forOrigin(baseUrl)

    override fun toString(): String =
        "DeviceEnrollmentLink(baseUrl=$baseUrl, enrollmentId=<redacted>, secret=<redacted>, appsAutomation=$appsAutomation)"
}

enum class AndroidAutomationTrustMode(val wireValue: String) {
    PlayIntegrity("play_integrity"),
    OwnerPinnedPrivateBuild("owner_pinned_private_build");

    companion object {
        fun fromWire(value: String?): AndroidAutomationTrustMode? =
            entries.firstOrNull { it.wireValue == value }
    }
}

internal fun retainedPlayIntegrityProjectNumber(
    trustMode: AndroidAutomationTrustMode,
    storedProjectNumber: Long,
): Long? = storedProjectNumber.takeIf {
    trustMode == AndroidAutomationTrustMode.PlayIntegrity && it > 0
}

enum class DeviceConnectionMode(val label: String) {
    SameWifi("Same Wi-Fi"),
    Remote("Remote");

    companion object {
        fun forOrigin(origin: String): DeviceConnectionMode =
            if (DeviceEnrollmentLinks.isSameWifiOrigin(origin)) SameWifi else Remote
    }
}

/**
 * Parses only the narrow URI contract Magician emits.
 *
 * A random web page can launch a custom scheme, so accepting arbitrary paths,
 * duplicated parameters, embedded credentials, or a non-HTTP destination here
 * would let it silently repoint a phone. The UI still asks the owner to confirm
 * the normalized host before this capability is exchanged.
 */
object DeviceEnrollmentLinks {
    fun parse(raw: String?): DeviceEnrollmentLink? {
        val value = raw?.trim()?.takeIf { it.length in 1..4096 } ?: return null
        val uri = runCatching { URI(value) }.getOrNull() ?: return null
        if (!MagicanAppLinks.isScheme(uri.scheme) || uri.host !in setOf("connect", "pair", "apps-connect") || !uri.path.isNullOrEmpty() || uri.fragment != null) {
            return null
        }
        val query = parseUniqueQuery(uri.rawQuery ?: return null) ?: return null
        val legacy = uri.host == "pair"
        val appsAutomation = uri.host == "apps-connect"
        val trustMode = if (appsAutomation) {
            AndroidAutomationTrustMode.fromWire(query["trust"]) ?: return null
        } else {
            null
        }
        val expected = when {
            legacy -> setOf("base", "id", "secret")
            trustMode == AndroidAutomationTrustMode.PlayIntegrity ->
                setOf("base", "id", "secret", "challenge", "trust", "project")
            appsAutomation -> setOf("base", "id", "secret", "challenge", "trust")
            else -> setOf("base", "id", "secret", "kind")
        }
        if (query.keys != expected) return null
        val clientKind = if (legacy || appsAutomation) "android" else query["kind"].orEmpty()
        if (clientKind != "android") return null

        val enrollmentId = query["id"].orEmpty()
        val secret = query["secret"].orEmpty()
        if (enrollmentId.length !in 16..128 || secret.length !in 32..256) return null

        val base = normalizedHttpOrigin(query["base"].orEmpty()) ?: return null
        val challenge = if (appsAutomation) {
            decodeEnrollmentChallenge(query["challenge"].orEmpty())
                ?.takeIf { it.size == 32 }
                ?: return null
        } else {
            null
        }
        val playProject = if (trustMode == AndroidAutomationTrustMode.PlayIntegrity) {
            query["project"]?.toLongOrNull()?.takeIf { it > 0 } ?: return null
        } else {
            null
        }
        return DeviceEnrollmentLink(
            base,
            enrollmentId,
            secret,
            clientKind,
            appsAutomation,
            challenge,
            playProject,
            trustMode,
        )
    }

    private fun parseUniqueQuery(raw: String): Map<String, String>? {
        val values = linkedMapOf<String, String>()
        for (piece in raw.split('&')) {
            val key = decode(piece.substringBefore('=')) ?: return null
            val value = decode(piece.substringAfter('=', "")) ?: return null
            if (key.isEmpty() || values.put(key, value) != null) return null
        }
        return values
    }

    private fun decode(value: String): String? = runCatching {
        URLDecoder.decode(value, Charsets.UTF_8.name())
    }.getOrNull()

    @OptIn(ExperimentalEncodingApi::class)
    private fun decodeEnrollmentChallenge(value: String): ByteArray? =
        runCatching { KotlinBase64.Default.decode(value) }.getOrNull()

    internal fun normalizedHttpOrigin(raw: String): String? {
        val uri = runCatching { URI(raw.trim()) }.getOrNull() ?: return null
        if (uri.scheme !in setOf("https", "http") || uri.host.isNullOrBlank() || uri.userInfo != null) return null
        val host = uri.host.lowercase()
        val privateHttp = uri.scheme == "http" && isPrivateNetworkHost(host)
        if (uri.scheme != "https" && !privateHttp) return null
        if (!uri.path.isNullOrEmpty() && uri.path != "/") return null
        if (uri.query != null || uri.fragment != null) return null
        val port = if (uri.port == -1) "" else ":${uri.port}"
        val renderedHost = if (host.contains(':')) "[$host]" else host
        return "${uri.scheme}://$renderedHost$port"
    }

    fun isSameWifiOrigin(raw: String): Boolean {
        val uri = runCatching { URI(raw) }.getOrNull() ?: return false
        return uri.scheme == "http" && uri.host?.let(::isPrivateNetworkHost) == true
    }

    private fun isPrivateNetworkHost(rawHost: String): Boolean {
        val host = rawHost.lowercase().removePrefix("[").removeSuffix("]")
        if (host == "localhost") return true
        // Resolve literals only. A hostname must never gain plaintext access
        // merely because DNS happened to return a private address.
        if (!host.contains(':') && host.any { it !in '0'..'9' && it != '.' }) return false
        return runCatching { InetAddress.getByName(host) }.getOrNull()?.let {
            it.isLoopbackAddress || it.isSiteLocalAddress || it.isLinkLocalAddress
        } == true
    }
}

/** Recovery can rotate credentials on the enrolled origin, never retarget them. */
internal fun requireRecoveryOrigin(current: String, requested: String): String {
    val enrolled = DeviceEnrollmentLinks.normalizedHttpOrigin(current)
    val candidate = DeviceEnrollmentLinks.normalizedHttpOrigin(requested)
    require(enrolled != null && candidate == enrolled) {
        "Changing servers requires a new owner-issued connection code."
    }
    return enrolled
}

/**
 * A regular mobile grant must echo the QR origin exactly. Apps automation
 * responses may also name the deployment's configured remote origin, but that
 * metadata cannot retarget the attested one-time request: the scanned origin
 * remains the authority used for grant verification and durable storage.
 */
internal fun enrollmentResponseOriginMatches(
    linkOrigin: String,
    responseOrigin: String,
    appsAutomation: Boolean,
): Boolean = appsAutomation || responseOrigin == linkOrigin

data class DeviceEnrollmentResult(
    val baseUrl: String,
    val principal: String,
    val workspace: String,
    val token: String,
    val cloudflareClientId: String,
    val cloudflareClientSecret: String,
    val automationKeyAlias: String? = null,
    val automationKeyId: String? = null,
    val automationApkSha256: String? = null,
    val automationAttestationPolicyDigest: String? = null,
    val automationPlayIntegrityCloudProjectNumber: Long? = null,
    val automationTrustMode: AndroidAutomationTrustMode? = null,
    val pendingAppsEnrollmentId: String? = null,
    val pendingAppsRequestSha256: String? = null,
) {
    override fun toString(): String =
        "DeviceEnrollmentResult(baseUrl=$baseUrl, principal=$principal, workspace=$workspace, credentials=<redacted>)"
}

class DeviceEnrollmentException(message: String) : Exception(message)

internal enum class PendingAppsEnrollmentDisposition {
    RetainForExactRetry,
    AbandonProvenUnspent,
}

internal fun enrollmentFailureMessage(
    status: Int,
    error: String?,
    appsAutomation: Boolean,
    baseUrl: String,
): String = when {
    status == 410 || error == "enrollment_invalid_or_expired" ->
        "This pairing code expired or was already used. Create a new one in Magician."
    appsAutomation && error == "android_apps_attestation_rejected" ->
        "This Android build or device did not match the reviewed hardware-attestation policy. Check the signer, app version, and attestation root shown in Magician Settings."
    appsAutomation && error == "android_play_integrity_rejected" ->
        "Google Play could not verify this release. Use Google Play release only for the Play-installed app, or choose Private / self-hosted build in Magician Settings."
    appsAutomation && error == "android_play_integrity_unavailable" ->
        "Google Play verification is temporarily unavailable. Retry, or choose Private / self-hosted build for an APK you installed yourself."
    appsAutomation && error == "android_private_build_token_unexpected" ->
        "The QR and Android app disagree about the trust method. Create a fresh App Pilot QR with the intended choice and scan it again."
    appsAutomation && error == "android_owner_proposal_pending" ->
        "Another Android owner approval is already open. Finish or cancel it in Magican Desktop, then create a fresh App Pilot QR."
    appsAutomation && error == "android_apps_owner_authorization_rejected" ->
        "Magican Desktop rejected or expired the owner approval. Create a fresh App Pilot enrollment and approve it on the same desktop."
    appsAutomation && error == "android_apps_owner_unavailable" ->
        "The desktop owner approval service is unavailable. Keep Magican Desktop connected, then retry."
    status == 401 || status == 403 ->
        "The secure access route did not allow enrollment at $baseUrl. Re-run mobile Access setup on the Magician host."
    else -> "Magician could not pair this phone (HTTP $status)."
}

/**
 * A timeout or cancellation after the first write may have reached Magician;
 * only an explicit Gone response or a failure before submission proves that
 * the retained key and connection secret can be destroyed.
 */
internal fun pendingAppsEnrollmentDisposition(
    requestMayHaveReachedServer: Boolean,
    createdPendingThisAttempt: Boolean,
    serverProvedUnspent: Boolean,
): PendingAppsEnrollmentDisposition =
    if (serverProvedUnspent || (!requestMayHaveReachedServer && createdPendingThisAttempt)) {
        PendingAppsEnrollmentDisposition.AbandonProvenUnspent
    } else {
        PendingAppsEnrollmentDisposition.RetainForExactRetry
    }

/** Exchanges a scanned one-time capability; its secret-bearing DTOs redact diagnostic output. */
class DeviceEnrollmentClient private constructor(
    private val clientFactory: () -> HttpClient,
    private val deviceId: String,
    private val deviceLabel: String,
    private val automationIdentity: AndroidAutomationIdentityManager?,
    private val appContext: Context?,
) {
    private val json = Json { ignoreUnknownKeys = true }

    constructor(context: Context) : this(
        clientFactory = ::defaultHttpClient,
        deviceId = MagicianAccess.deviceId(context.applicationContext),
        deviceLabel = "${Build.MANUFACTURER} ${Build.MODEL}".trim(),
        automationIdentity = AndroidAutomationIdentityManager(context.applicationContext),
        appContext = context.applicationContext,
    )

    internal constructor(client: HttpClient, deviceId: String, deviceLabel: String) : this(
        clientFactory = { client },
        deviceId = deviceId,
        deviceLabel = deviceLabel,
        automationIdentity = null,
        appContext = null,
    )

    /**
     * Reconstruct only the routing capability needed to replay an already
     * sealed Apps request. The retained request body, Keystore alias, and
     * connection credential remain inside the bridge module.
     */
    fun pendingAppsEnrollmentLink(): DeviceEnrollmentLink? {
        val context = appContext ?: return null
        val pending = MagicianAccess.pendingAppsEnrollment(context) ?: return null
        return DeviceEnrollmentLink(
            baseUrl = pending.baseUrl,
            enrollmentId = pending.enrollmentId,
            secret = pending.enrollmentSecret,
            appsAutomation = true,
            challenge = Base64.decode(pending.challengeBase64, Base64.DEFAULT),
            playIntegrityCloudProjectNumber = retainedPlayIntegrityProjectNumber(
                pending.automationTrustMode,
                pending.playIntegrityCloudProjectNumber,
            ),
            automationTrustMode = pending.automationTrustMode,
        )
    }

    suspend fun exchange(
        link: DeviceEnrollmentLink,
        onProgress: (String) -> Unit = {},
    ): DeviceEnrollmentResult {
        val client = clientFactory()
        var automationProof: AndroidAutomationEnrollmentProof? = null
        var automationConnectionSecret: String? = null
        var automationKeyAlias: String? = null
        var automationKeyId: String? = null
        var automationApkSha256: String? = null
        var appsRequestSha256: String? = null
        var requestMayHaveReachedServer = false
        var createdPendingThisAttempt = false
        var serverProvedUnspent = false
        try {
            val pendingAppsEnrollment = appContext?.let(MagicianAccess::pendingAppsEnrollment)
            if (!link.appsAutomation && pendingAppsEnrollment != null) {
                throw DeviceEnrollmentException(
                    "An Android Apps enrollment is awaiting exact recovery and must be resolved first.",
                )
            }
            val retained = pendingAppsEnrollment.takeIf { link.appsAutomation }
            if (retained != null) {
                if (retained.baseUrl != link.baseUrl || retained.enrollmentId != link.enrollmentId ||
                    retained.enrollmentSecret != link.secret ||
                    (link.challenge != null &&
                        retained.enrollmentRequestSha256 != enrollmentRequestDigest(link))) {
                    throw DeviceEnrollmentException(
                        "A different Android Apps enrollment is awaiting exact recovery.",
                    )
                }
                automationConnectionSecret = retained.connectionSecret
                automationKeyAlias = retained.keyAlias
                automationKeyId = retained.keyId
                automationApkSha256 = retained.apkSha256
                appsRequestSha256 = retained.requestSha256
            } else if (link.appsAutomation) {
                val owner = automationIdentity
                    ?: throw DeviceEnrollmentException("Hardware-backed Apps enrollment is unavailable in this client.")
                onProgress("Creating hardware-backed phone identity…")
                automationConnectionSecret = randomConnectionSecret()
                val connectionSecretSha256 = sha256Hex(automationConnectionSecret)
                automationProof = owner.createEnrollmentProof(
                    link,
                    deviceId,
                    deviceLabel,
                    connectionSecretSha256,
                )
                automationKeyAlias = automationProof.keyAlias
                automationKeyId = automationProof.keyId
                automationApkSha256 = automationProof.apkSha256
                onProgress("Preparing signed App Pilot enrollment…")
            }
            val endpoint = if (link.appsAutomation) {
                "/api/magician/v2/devices/apps-automation/enrollment/exchange"
            } else {
                "/api/magician/v2/devices/enrollment/exchange"
            }
            var requestBody = retained?.requestBody ?: if (automationProof != null) {
                    val proof = requireNotNull(automationProof)
                    val challenge = requireNotNull(link.challenge)
                    val signedMaterial = AndroidAutomationIdentityManager.enrollmentSigningBytes(
                        link.enrollmentId,
                        deviceId,
                        deviceLabel,
                        proof.keyId,
                        proof.publicKeySpkiBase64,
                        proof.appPackage,
                        proof.appVersionCode,
                        proof.appSigningSha256,
                        proof.apkSha256,
                        proof.connectionSecretSha256,
                        challenge,
                    )
                    val signature = Base64.decode(proof.signatureBase64, Base64.DEFAULT)
                    val integrityToken = if (link.automationTrustMode == AndroidAutomationTrustMode.PlayIntegrity) {
                        AndroidPlayIntegrity.token(
                            requireNotNull(appContext),
                            requireNotNull(link.playIntegrityCloudProjectNumber),
                            AndroidPlayIntegrity.requestHash(
                                "magician.android-play-integrity.enrollment.v1",
                                signedMaterial,
                                signature,
                            ),
                        )
                    } else {
                        ""
                    }
                    json.encodeToString(
                        AppsExchangeRequest.serializer(),
                        AppsExchangeRequest(
                            enrollmentId = link.enrollmentId,
                            secret = link.secret,
                            deviceId = deviceId,
                            label = deviceLabel,
                            keyId = proof.keyId,
                            publicKeySpkiBase64 = proof.publicKeySpkiBase64,
                            certificateChainBase64 = proof.certificateChainBase64,
                            appPackage = proof.appPackage,
                            appVersionCode = proof.appVersionCode,
                            appSigningSha256 = proof.appSigningSha256,
                            apkSha256 = proof.apkSha256,
                            connectionSecretSha256 = proof.connectionSecretSha256,
                            signatureBase64 = proof.signatureBase64,
                            playIntegrityToken = integrityToken,
                        ),
                    )
                } else {
                    json.encodeToString(
                        ExchangeRequest.serializer(),
                        ExchangeRequest(
                            enrollmentId = link.enrollmentId,
                            secret = link.secret,
                            deviceId = deviceId,
                            label = deviceLabel,
                        ),
                    )
                }
            if (link.appsAutomation && retained == null) {
                val context = appContext
                    ?: throw DeviceEnrollmentException("Secure Apps enrollment storage is unavailable.")
                val proof = requireNotNull(automationProof)
                appsRequestSha256 = sha256Hex(requestBody)
                try {
                    MagicianAccess.retainPendingAppsEnrollment(
                        context,
                        PendingAppsEnrollmentRetry(
                            baseUrl = link.baseUrl,
                            enrollmentId = link.enrollmentId,
                            enrollmentSecret = link.secret,
                            enrollmentRequestSha256 = enrollmentRequestDigest(link),
                            requestBody = requestBody,
                            requestSha256 = requireNotNull(appsRequestSha256),
                            connectionSecret = requireNotNull(automationConnectionSecret),
                            connectionSecretSha256 = proof.connectionSecretSha256,
                            keyAlias = proof.keyAlias,
                            keyId = proof.keyId,
                            apkSha256 = proof.apkSha256,
                            playIntegrityCloudProjectNumber = link.playIntegrityCloudProjectNumber ?: 0L,
                            automationTrustMode = requireNotNull(link.automationTrustMode),
                            challengeBase64 = Base64.encodeToString(
                                requireNotNull(link.challenge),
                                Base64.NO_WRAP,
                            ),
                        ),
                    )
                } catch (error: Throwable) {
                    // No request was submitted. A failed SharedPreferences
                    // commit is nevertheless ambiguous about its in-process
                    // publication, so delete the key only after the exact
                    // journal is absent or was durably cleared. If clearing is
                    // itself ambiguous, retaining both is the only retry-safe
                    // outcome.
                    val retainedAfterFailure = runCatching {
                        MagicianAccess.pendingAppsEnrollment(context)
                    }.getOrNull()?.takeIf {
                        it.enrollmentId == link.enrollmentId &&
                            it.requestSha256 == appsRequestSha256 &&
                            it.keyAlias == proof.keyAlias
                    }
                    val aliasSafeToDiscard = if (retainedAfterFailure == null) {
                        proof.keyAlias
                    } else {
                        runCatching {
                            MagicianAccess.abandonPendingAppsEnrollment(
                                context,
                                link.enrollmentId,
                                requireNotNull(appsRequestSha256),
                            )
                        }.getOrNull()
                    }
                    aliasSafeToDiscard?.let { automationIdentity?.discard(it) }
                    throw error
                }
                createdPendingThisAttempt = true
            }
            if (link.appsAutomation && retained != null) {
                val retainedRequest = runCatching {
                    json.decodeFromString(AppsExchangeRequest.serializer(), requestBody)
                }.getOrElse {
                    throw DeviceEnrollmentException("The retained Apps enrollment request is unreadable.")
                }
                if (link.automationTrustMode == AndroidAutomationTrustMode.PlayIntegrity) {
                    val signedMaterial = AndroidAutomationIdentityManager.enrollmentSigningBytes(
                        retainedRequest.enrollmentId,
                        retainedRequest.deviceId,
                        retainedRequest.label,
                        retainedRequest.keyId,
                        retainedRequest.publicKeySpkiBase64,
                        retainedRequest.appPackage,
                        retainedRequest.appVersionCode,
                        retainedRequest.appSigningSha256,
                        retainedRequest.apkSha256,
                        retainedRequest.connectionSecretSha256,
                        requireNotNull(link.challenge),
                    )
                    val nextToken = AndroidPlayIntegrity.token(
                        requireNotNull(appContext),
                        requireNotNull(link.playIntegrityCloudProjectNumber),
                        AndroidPlayIntegrity.requestHash(
                            "magician.android-play-integrity.enrollment.v1",
                            signedMaterial,
                            Base64.decode(retainedRequest.signatureBase64, Base64.DEFAULT),
                        ),
                    )
                    requestBody = json.encodeToString(
                        AppsExchangeRequest.serializer(),
                        retainedRequest.copy(playIntegrityToken = nextToken),
                    )
                    appsRequestSha256 = MagicianAccess.rotatePendingAppsIntegrityToken(
                        requireNotNull(appContext),
                        link.enrollmentId,
                        retained.requestSha256,
                        requestBody,
                    )
                }
            }
            requestMayHaveReachedServer = true
            onProgress("Contacting Magician…")
            var response = client.post("${link.baseUrl}$endpoint") {
                contentType(ContentType.Application.Json)
                setBody(requestBody)
            }
            var body = response.bodyAsText()
            var pendingPolls = 0
            while (link.appsAutomation && response.status.value == 202) {
                val status = runCatching {
                    json.decodeFromString(AppsActivationResponse.serializer(), body).status
                }.getOrNull()
                if (status != "owner_approval_pending" || pendingPolls++ >= MAX_OWNER_APPROVAL_POLLS) {
                    throw DeviceEnrollmentException("Magician did not complete the desktop owner approval.")
                }
                onProgress("Waiting for approval in Magican Desktop…")
                delay(OWNER_APPROVAL_POLL_DELAY_MS)
                response = client.post("${link.baseUrl}$endpoint") {
                    contentType(ContentType.Application.Json)
                    setBody(requestBody)
                }
                body = response.bodyAsText()
            }
            if (!response.status.isSuccess()) {
                serverProvedUnspent = link.appsAutomation && response.status.value == 410
                val error = runCatching { json.decodeFromString(ErrorResponse.serializer(), body).error }
                    .getOrNull()
                val message = enrollmentFailureMessage(
                    response.status.value,
                    error,
                    link.appsAutomation,
                    link.baseUrl,
                )
                throw DeviceEnrollmentException(message)
            }
            val grant = runCatching { json.decodeFromString(ExchangeResponse.serializer(), body) }
                .getOrElse { throw DeviceEnrollmentException("Magician returned an unreadable pairing response.") }
            if (
                (!link.appsAutomation && grant.token.isBlank()) ||
                (link.appsAutomation && grant.status != "active") ||
                grant.principal.isBlank() || grant.workspace.isBlank() ||
                !enrollmentResponseOriginMatches(
                    link.baseUrl,
                    grant.publicOrigin,
                    link.appsAutomation,
                ) || grant.clientKind != "android" ||
                "mobile_client" !in grant.capabilities ||
                    (link.appsAutomation && "device_automation" !in grant.capabilities) ||
                    (!link.appsAutomation && "device_automation" in grant.capabilities) ||
                    (link.appsAutomation && grant.keyId != automationKeyId) ||
                    (link.appsAutomation && grant.apkSha256 != automationApkSha256) ||
                (link.appsAutomation && !grant.attestationPolicyDigest.startsWith("blake3:"))
            ) {
                throw DeviceEnrollmentException("Magician returned an incomplete pairing response.")
            }
            val effectiveToken = automationConnectionSecret ?: grant.token
            verifyGrant(
                client = client,
                link = link,
                grant = grant.copy(token = effectiveToken),
                deviceId = deviceId,
            )
            return DeviceEnrollmentResult(
                baseUrl = link.baseUrl,
                principal = grant.principal,
                workspace = grant.workspace,
                token = effectiveToken,
                cloudflareClientId = grant.cloudflareAccess?.clientId.orEmpty(),
                cloudflareClientSecret = grant.cloudflareAccess?.clientSecret.orEmpty(),
                automationKeyAlias = automationKeyAlias,
                automationKeyId = automationKeyId,
                automationApkSha256 = automationApkSha256,
                automationAttestationPolicyDigest = grant.attestationPolicyDigest.takeIf { link.appsAutomation },
                automationPlayIntegrityCloudProjectNumber = link.playIntegrityCloudProjectNumber.takeIf { link.appsAutomation },
                automationTrustMode = link.automationTrustMode.takeIf { link.appsAutomation },
                pendingAppsEnrollmentId = link.enrollmentId.takeIf { link.appsAutomation },
                pendingAppsRequestSha256 = appsRequestSha256,
            )
        } catch (cancelled: CancellationException) {
            if (pendingAppsEnrollmentDisposition(
                    requestMayHaveReachedServer,
                    createdPendingThisAttempt,
                    serverProvedUnspent,
                ) == PendingAppsEnrollmentDisposition.AbandonProvenUnspent) {
                abandonRetainedAppsEnrollment(link, appsRequestSha256)
            }
            throw cancelled
        } catch (known: DeviceEnrollmentException) {
            if (pendingAppsEnrollmentDisposition(
                    requestMayHaveReachedServer,
                    createdPendingThisAttempt,
                    serverProvedUnspent,
                ) == PendingAppsEnrollmentDisposition.AbandonProvenUnspent) {
                abandonRetainedAppsEnrollment(link, appsRequestSha256)
            }
            throw known
        } catch (error: Throwable) {
            if (pendingAppsEnrollmentDisposition(
                    requestMayHaveReachedServer,
                    createdPendingThisAttempt,
                    serverProvedUnspent,
                ) == PendingAppsEnrollmentDisposition.AbandonProvenUnspent) {
                abandonRetainedAppsEnrollment(link, appsRequestSha256)
            }
            throw DeviceEnrollmentException(
                if (error is java.net.ConnectException) {
                    "Magician is offline or unreachable at ${link.baseUrl}."
                } else {
                    "Could not pair with Magician: ${error.message ?: "connection failed"}"
                },
            )
        } finally {
            client.close()
        }
    }

    /**
     * Prove that both authentication layers work before the caller atomically
     * replaces its previous connection. The exchange path is intentionally a
     * narrow Cloudflare bypass, so a successful exchange alone cannot prove
     * the ordinary API path or the returned outer credential is usable.
     */
    private suspend fun verifyGrant(
        client: HttpClient,
        link: DeviceEnrollmentLink,
        grant: ExchangeResponse,
        deviceId: String,
    ) {
        val response = client.get("${link.baseUrl}/api/magician/v2/devices/me") {
            header(MagicianAccess.HEADER_DEVICE_ID, deviceId)
            header(MagicianAccess.HEADER_AUTHORIZATION, "Bearer ${grant.token}")
            grant.cloudflareAccess?.let { access ->
                if (access.clientId.isNotBlank() && access.clientSecret.isNotBlank()) {
                    header(MagicianAccess.HEADER_CLIENT_ID, access.clientId)
                    header(MagicianAccess.HEADER_CLIENT_SECRET, access.clientSecret)
                }
            }
        }
        val body = response.bodyAsText()
        val verified = if (response.status.isSuccess()) {
            runCatching { json.decodeFromString(VerificationResponse.serializer(), body) }.getOrNull()
        } else {
            null
        }
        if (
            verified?.deviceId != deviceId ||
            verified.principal != grant.principal ||
            verified.workspace != grant.workspace
        ) {
            throw DeviceEnrollmentException(
                "The new Magician connection could not be verified. Your previous connection was kept.",
            )
        }
    }

    companion object {
        private const val OWNER_APPROVAL_POLL_DELAY_MS = 2_000L
        private const val MAX_OWNER_APPROVAL_POLLS = 150

        private fun randomConnectionSecret(): String {
            val bytes = ByteArray(32).also(SecureRandom()::nextBytes)
            return Base64.encodeToString(
                bytes,
                Base64.URL_SAFE or Base64.NO_WRAP or Base64.NO_PADDING,
            )
        }

        private fun sha256Hex(value: String): String = MessageDigest.getInstance("SHA-256")
            .digest(value.toByteArray(Charsets.UTF_8))
            .joinToString("") { byte -> "%02x".format(byte) }

        private fun enrollmentRequestDigest(link: DeviceEnrollmentLink): String {
            val challenge = link.challenge
                ?.let { Base64.encodeToString(it, Base64.NO_WRAP) }
                .orEmpty()
            return sha256Hex(
                listOf(
                    "magician.android-apps-enrollment-request.v1",
                    link.baseUrl,
                    link.enrollmentId,
                    link.secret,
                    challenge,
                    link.automationTrustMode?.wireValue.orEmpty(),
                    link.playIntegrityCloudProjectNumber?.toString().orEmpty(),
                ).joinToString("\u0000"),
            )
        }

        private fun defaultHttpClient(): HttpClient = HttpClient(CIO) {
            install(HttpTimeout) {
                connectTimeoutMillis = 10_000
                requestTimeoutMillis = 20_000
            }
        }
    }

    private fun abandonRetainedAppsEnrollment(link: DeviceEnrollmentLink, requestSha256: String?) {
        val context = appContext ?: return
        val digest = requestSha256 ?: return
        MagicianAccess.abandonPendingAppsEnrollment(context, link.enrollmentId, digest)
            ?.let { alias -> automationIdentity?.discard(alias) }
    }
}

@Serializable
private data class ExchangeRequest(
    @SerialName("enrollment_id") val enrollmentId: String,
    val secret: String,
    @SerialName("device_id") val deviceId: String,
    val label: String,
)

@Serializable
private data class AppsExchangeRequest(
    @SerialName("enrollment_id") val enrollmentId: String,
    val secret: String,
    @SerialName("device_id") val deviceId: String,
    val label: String,
    @SerialName("key_id") val keyId: String,
    @SerialName("public_key_spki_base64") val publicKeySpkiBase64: String,
    @SerialName("certificate_chain_base64") val certificateChainBase64: List<String>,
    @SerialName("app_package") val appPackage: String,
    @SerialName("app_version_code") val appVersionCode: Long,
    @SerialName("app_signing_sha256") val appSigningSha256: String,
    @SerialName("apk_sha256") val apkSha256: String,
    @SerialName("connection_secret_sha256") val connectionSecretSha256: String,
    @SerialName("signature_base64") val signatureBase64: String,
    @SerialName("play_integrity_token") val playIntegrityToken: String,
)

@Serializable
private data class ExchangeResponse(
    val status: String = "",
    val token: String = "",
    val principal: String = "",
    val workspace: String = "",
    @SerialName("public_origin") val publicOrigin: String = "",
    @SerialName("client_kind") val clientKind: String = "",
    val capabilities: List<String> = emptyList(),
    @SerialName("key_id") val keyId: String = "",
    @SerialName("apk_sha256") val apkSha256: String = "",
    @SerialName("attestation_policy_digest") val attestationPolicyDigest: String = "",
    @SerialName("cloudflare_access") val cloudflareAccess: CloudflareAccessResponse? = null,
)

@Serializable
private data class AppsActivationResponse(val status: String = "")

@Serializable
private data class CloudflareAccessResponse(
    @SerialName("client_id") val clientId: String = "",
    @SerialName("client_secret") val clientSecret: String = "",
)

@Serializable
private data class VerificationResponse(
    @SerialName("device_id") val deviceId: String = "",
    val principal: String = "",
    val workspace: String = "",
)

@Serializable
private data class ErrorResponse(val error: String = "")
