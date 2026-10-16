package ai.magicbeans.magdroid.access

import android.content.Context
import android.content.pm.PackageManager
import android.content.pm.Checksum
import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import android.util.Log
import java.io.ByteArrayOutputStream
import java.io.FileInputStream
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.security.KeyPairGenerator
import java.security.KeyStore
import java.security.MessageDigest
import java.security.SecureRandom
import java.security.Signature
import java.security.spec.ECGenParameterSpec
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull

/** Move-only enrollment material; certificate evidence never enters preferences. */
internal data class AndroidAutomationEnrollmentProof(
    val keyAlias: String,
    val keyId: String,
    val publicKeySpkiBase64: String,
    val certificateChainBase64: List<String>,
    val appPackage: String,
    val appVersionCode: Long,
    val appSigningSha256: String,
    val apkSha256: String,
    val connectionSecretSha256: String,
    val signatureBase64: String,
)

/** Owns the hardware-backed Apps key used for enrollment and every socket generation. */
class AndroidAutomationIdentityManager(private val context: Context) {
    internal suspend fun createEnrollmentProof(
        link: DeviceEnrollmentLink,
        deviceId: String,
        label: String,
        connectionSecretSha256: String,
    ): AndroidAutomationEnrollmentProof {
        require(link.appsAutomation && link.challenge != null)
        require(
            connectionSecretSha256.length == 64 &&
                connectionSecretSha256.all { character ->
                    character in '0'..'9' || character.lowercaseChar() in 'a'..'f'
                },
        )
        val keyId = randomKeyId()
        val alias = "$KEY_ALIAS_PREFIX$keyId"
        Log.i(TAG, "Creating challenge-bound Android Keystore identity")
        val generator = KeyPairGenerator.getInstance(KeyProperties.KEY_ALGORITHM_EC, ANDROID_KEYSTORE)
        generator.initialize(
            KeyGenParameterSpec.Builder(alias, KeyProperties.PURPOSE_SIGN)
                .setAlgorithmParameterSpec(ECGenParameterSpec("secp256r1"))
                .setDigests(KeyProperties.DIGEST_SHA256)
                .setAttestationChallenge(link.challenge)
                .setUserAuthenticationRequired(false)
                .build(),
        )
        val pair = generator.generateKeyPair()
        Log.i(TAG, "Android Keystore identity created")
        try {
            val store = keyStore()
            val chain = store.getCertificateChain(alias)?.toList().orEmpty()
            if (chain.isEmpty() || chain.size > MAX_CERTIFICATES) {
                throw DeviceEnrollmentException("Android did not return a bounded hardware attestation chain.")
            }
            val spki = Base64.encodeToString(pair.public.encoded, Base64.NO_WRAP)
            val appVersionCode = applicationVersionCode()
            val signingDigest = applicationSigningDigest()
            Log.i(TAG, "Resolving installed APK identity")
            val apkDigest = applicationApkSha256(
                allowInstalledApkFallback =
                    link.automationTrustMode == AndroidAutomationTrustMode.OwnerPinnedPrivateBuild,
            )
            Log.i(TAG, "Installed APK identity resolved")
            val signingBytes = enrollmentSigningBytes(
                link.enrollmentId,
                deviceId,
                label,
                keyId,
                spki,
                context.packageName,
                appVersionCode,
                signingDigest,
                apkDigest,
                connectionSecretSha256,
                link.challenge,
            )
            Log.i(TAG, "Signing Apps enrollment proof")
            val signature = Signature.getInstance("SHA256withECDSA").run {
                initSign(pair.private)
                update(signingBytes)
                sign()
            }
            Log.i(TAG, "Apps enrollment proof signed")
            return AndroidAutomationEnrollmentProof(
                keyAlias = alias,
                keyId = keyId,
                publicKeySpkiBase64 = spki,
                certificateChainBase64 = chain.map { certificate ->
                    val bytes = certificate.encoded
                    if (bytes.size > MAX_CERTIFICATE_BYTES) {
                        throw DeviceEnrollmentException("Android returned an oversized attestation certificate.")
                    }
                    Base64.encodeToString(bytes, Base64.NO_WRAP)
                },
                appPackage = context.packageName,
                appVersionCode = appVersionCode,
                appSigningSha256 = signingDigest,
                apkSha256 = apkDigest,
                connectionSecretSha256 = connectionSecretSha256,
                signatureBase64 = Base64.encodeToString(signature, Base64.NO_WRAP),
            )
        } catch (error: Throwable) {
            discard(alias)
            throw error
        }
    }

    internal suspend fun signSocketProof(
        keyAlias: String,
        connectionId: String,
        keyId: String,
        targetRef: String,
        reviewGeneration: Long,
        protocolVersion: String,
        serverNonce: ByteArray,
        expectedApkSha256: String,
        attestationPolicyDigest: String,
        automationTrustMode: AndroidAutomationTrustMode,
    ): String {
        require(connectionId.length <= 64 && keyId.length in 16..128 && targetRef.length <= 256)
        require(reviewGeneration >= 0 && protocolVersion.length <= 64 && serverNonce.size == 32)
        require(expectedApkSha256.length == 64 && attestationPolicyDigest.startsWith("blake3:"))
        val currentApkSha256 = applicationApkSha256(
            allowInstalledApkFallback =
                automationTrustMode == AndroidAutomationTrustMode.OwnerPinnedPrivateBuild,
        )
        if (currentApkSha256 != expectedApkSha256) {
            throw IllegalStateException("The installed Magdroid APK no longer matches its Apps enrollment")
        }
        val key = keyStore().getKey(keyAlias, null)
            ?: throw IllegalStateException("The enrolled Apps key is unavailable")
        val signature = Signature.getInstance("SHA256withECDSA").run {
            initSign(key as java.security.PrivateKey)
            update(
                socketSigningBytes(
                    connectionId,
                    keyId,
                    targetRef,
                    reviewGeneration,
                    protocolVersion,
                    serverNonce,
                    currentApkSha256,
                    attestationPolicyDigest,
                ),
            )
            sign()
        }
        return Base64.encodeToString(signature, Base64.NO_WRAP)
    }

    internal fun discard(alias: String) {
        if (alias.startsWith(KEY_ALIAS_PREFIX)) {
            runCatching { keyStore().deleteEntry(alias) }
        }
    }

    private fun applicationSigningDigest(): String {
        val packageInfo = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            context.packageManager.getPackageInfo(
                context.packageName,
                PackageManager.GET_SIGNING_CERTIFICATES,
            )
        } else {
            @Suppress("DEPRECATION")
            context.packageManager.getPackageInfo(context.packageName, PackageManager.GET_SIGNATURES)
        }
        val signatures = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            val signingInfo = packageInfo.signingInfo
            if (signingInfo.hasMultipleSigners()) signingInfo.apkContentsSigners
            else signingInfo.signingCertificateHistory
        } else {
            @Suppress("DEPRECATION")
            packageInfo.signatures
        }
        if (signatures.size != 1) {
            throw DeviceEnrollmentException("This Magdroid signing lineage is not supported for Apps enrollment.")
        }
        return MessageDigest.getInstance("SHA-256")
            .digest(signatures.single().toByteArray())
            .joinToString("") { byte -> "%02x".format(byte) }
    }

    private fun applicationVersionCode(): Long {
        val packageInfo = context.packageManager.getPackageInfo(context.packageName, 0)
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            packageInfo.longVersionCode
        } else {
            @Suppress("DEPRECATION")
            packageInfo.versionCode.toLong()
        }
    }

    /** Resolve the installed whole-APK checksum; split APKs remain outside V1. */
    private suspend fun applicationApkSha256(allowInstalledApkFallback: Boolean = false): String {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.S) {
            throw DeviceEnrollmentException("Android 12 or newer is required for exact Apps artifact verification.")
        }
        val applicationInfo = context.packageManager.getApplicationInfo(context.packageName, 0)
        if (!applicationInfo.splitSourceDirs.isNullOrEmpty() || !applicationInfo.splitNames.isNullOrEmpty()) {
            throw DeviceEnrollmentException(
                "Split APKs are not admitted by the Android Apps V1 artifact policy.",
            )
        }
        // The reviewed Apps contract pins the ordinary whole-file SHA-256, not
        // the APK Signature Scheme V4 Merkle root. A private build hashes the
        // exact installed base-APK path selected by PackageManager; this avoids
        // vendor implementations whose checksum callback never returns for an
        // ADB-installed APK. Play-distributed enrollment keeps PackageManager's
        // platform computation and refuses installer-supplied values.
        if (allowInstalledApkFallback) {
            Log.i(TAG, "Hashing PackageManager's installed private-build APK path")
            return installedApkSha256(applicationInfo.sourceDir)
        }
        @Suppress("DEPRECATION")
        val reviewedDigestType = Checksum.TYPE_WHOLE_SHA256
        val platformDigest = withTimeoutOrNull(PLATFORM_CHECKSUM_TIMEOUT_MS) {
            requestPackageManagerChecksum(reviewedDigestType)
        }
        if (platformDigest != null) return platformDigest
        throw DeviceEnrollmentException(
            "Android did not finish exact APK verification. Retry after restarting the phone.",
        )
    }

    private suspend fun installedApkSha256(sourcePath: String): String =
        withContext(Dispatchers.IO) {
            val digest = MessageDigest.getInstance("SHA-256")
            FileInputStream(sourcePath).use { input ->
                val buffer = ByteArray(APK_HASH_BUFFER_BYTES)
                while (true) {
                    val count = input.read(buffer)
                    if (count < 0) break
                    if (count > 0) digest.update(buffer, 0, count)
                }
            }
            digest.digest().joinToString("") { byte -> "%02x".format(byte) }
        }

    private suspend fun requestPackageManagerChecksum(reviewedDigestType: Int): String =
        suspendCancellableCoroutine { continuation ->
            try {
                context.packageManager.requestChecksums(
                    context.packageName,
                    false,
                    reviewedDigestType,
                    // TRUST_ALL admits installer-supplied values that the
                    // platform explicitly does not verify. Apps authority
                    // accepts only PackageManager's own whole-APK digest.
                    PackageManager.TRUST_NONE,
                ) { checksums ->
                    if (!continuation.isActive) return@requestChecksums
                    val exact = checksums.filter {
                        it.type == reviewedDigestType && it.splitName == null
                    }
                    if (exact.size != 1 || exact.single().value.size != 32) {
                        continuation.resumeWithException(
                            DeviceEnrollmentException("Android did not return one exact whole-APK SHA-256 checksum."),
                        )
                    } else {
                        continuation.resume(
                            exact.single().value.joinToString("") { byte -> "%02x".format(byte) },
                        )
                    }
                }
            } catch (error: Throwable) {
                if (continuation.isActive) continuation.resumeWithException(error)
            }
        }

    private fun keyStore(): KeyStore = KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) }

    companion object {
        private const val ANDROID_KEYSTORE = "AndroidKeyStore"
        private const val KEY_ALIAS_PREFIX = "magdroid_apps_owner_v1_"
        private const val MAX_CERTIFICATES = 8
        private const val MAX_CERTIFICATE_BYTES = 16 * 1024
        private const val PLATFORM_CHECKSUM_TIMEOUT_MS = 5_000L
        private const val APK_HASH_BUFFER_BYTES = 64 * 1024
        private const val TAG = "MagdroidAppsIdentity"

        private fun randomKeyId(): String {
            val bytes = ByteArray(24).also(SecureRandom()::nextBytes)
            return Base64.encodeToString(bytes, Base64.URL_SAFE or Base64.NO_WRAP or Base64.NO_PADDING)
        }

        internal fun enrollmentSigningBytes(
            enrollmentId: String,
            deviceId: String,
            label: String,
            keyId: String,
            publicKeySpkiBase64: String,
            appPackage: String,
            appVersionCode: Long,
            appSigningSha256: String,
            apkSha256: String,
            connectionSecretSha256: String,
            challenge: ByteArray,
        ): ByteArray = framed(
            "magician.android-apps-enrollment-proof.v2\u0000".toByteArray(),
            enrollmentId.toByteArray(),
            deviceId.toByteArray(),
            label.toByteArray(),
            keyId.toByteArray(),
            publicKeySpkiBase64.toByteArray(),
            appPackage.toByteArray(),
            ByteBuffer.allocate(Long.SIZE_BYTES).order(ByteOrder.LITTLE_ENDIAN).putLong(appVersionCode).array(),
            appSigningSha256.toByteArray(),
            apkSha256.toByteArray(),
            connectionSecretSha256.toByteArray(),
            challenge,
        )

        internal fun socketSigningBytes(
            connectionId: String,
            keyId: String,
            targetRef: String,
            reviewGeneration: Long,
            protocolVersion: String,
            serverNonce: ByteArray,
            apkSha256: String,
            attestationPolicyDigest: String,
        ): ByteArray = framed(
            "magician.android-apps-socket-proof.v1\u0000".toByteArray(),
            connectionId.toByteArray(),
            keyId.toByteArray(),
            targetRef.toByteArray(),
            ByteBuffer.allocate(Long.SIZE_BYTES).order(ByteOrder.LITTLE_ENDIAN).putLong(reviewGeneration).array(),
            protocolVersion.toByteArray(),
            serverNonce,
            apkSha256.toByteArray(),
            attestationPolicyDigest.toByteArray(),
        )

        private fun framed(domain: ByteArray, vararg components: ByteArray): ByteArray =
            ByteArrayOutputStream().use { output ->
                output.write(domain)
                components.forEach { component ->
                    output.write(
                        ByteBuffer.allocate(Long.SIZE_BYTES)
                            .order(ByteOrder.LITTLE_ENDIAN)
                            .putLong(component.size.toLong())
                            .array(),
                    )
                    output.write(component)
                }
                output.toByteArray()
            }
    }
}
