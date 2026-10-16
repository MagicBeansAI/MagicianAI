package ai.magicbeans.magdroid.access

import android.content.Context
import android.os.SystemClock
import android.util.Base64
import com.google.android.play.core.integrity.IntegrityManagerFactory
import com.google.android.play.core.integrity.StandardIntegrityManager
import java.security.MessageDigest
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import kotlinx.coroutines.delay
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withTimeout

/** Produces opaque standard tokens; only Magician's server decodes verdicts. */
internal object AndroidPlayIntegrity {
    private val providerLock = Mutex()
    private var providerProject: Long? = null
    private var provider: StandardIntegrityManager.StandardIntegrityTokenProvider? = null
    private var prepareRetryAtElapsedMs: Long = 0

    suspend fun token(context: Context, cloudProjectNumber: Long, requestHash: String): String {
        require(cloudProjectNumber > 0 && requestHash.length == 43)
        return withTimeout(TOKEN_OPERATION_TIMEOUT_MS) {
            providerLock.withLock {
                if (providerProject != cloudProjectNumber) {
                    provider = null
                    providerProject = cloudProjectNumber
                    prepareRetryAtElapsedMs = 0
                }
                val waitMs = prepareRetryAtElapsedMs - SystemClock.elapsedRealtime()
                if (provider == null && waitMs > 0) {
                    delay(waitMs.coerceAtMost(PREPARE_BACKOFF_MS))
                }
                try {
                    val manager = if (provider == null) {
                        IntegrityManagerFactory.createStandard(context.applicationContext)
                    } else {
                        null
                    }
                    val currentProvider = provider ?: suspendCancellableCoroutine { continuation ->
                        requireNotNull(manager).prepareIntegrityToken(
                            StandardIntegrityManager.PrepareIntegrityTokenRequest.builder()
                                .setCloudProjectNumber(cloudProjectNumber)
                                .build(),
                        ).addOnSuccessListener { value ->
                            if (continuation.isActive) continuation.resume(value)
                        }.addOnFailureListener { error ->
                            if (continuation.isActive) continuation.resumeWithException(error)
                        }
                    }.also {
                        provider = it
                        prepareRetryAtElapsedMs = 0
                    }
                    val token = suspendCancellableCoroutine { continuation ->
                        currentProvider.request(
                            StandardIntegrityManager.StandardIntegrityTokenRequest.builder()
                                .setRequestHash(requestHash)
                                .build(),
                        ).addOnSuccessListener { response ->
                            if (continuation.isActive) continuation.resume(response.token())
                        }.addOnFailureListener { error ->
                            if (continuation.isActive) continuation.resumeWithException(error)
                        }
                    }
                    token.takeIf { it.length in 1..32 * 1024 }
                        ?: throw DeviceEnrollmentException("Play Integrity returned an invalid token.")
                } catch (error: Throwable) {
                    provider = null
                    prepareRetryAtElapsedMs = SystemClock.elapsedRealtime() + PREPARE_BACKOFF_MS
                    throw error
                }
            }
        }
    }

    internal fun requestHash(domain: String, signedMaterial: ByteArray, signature: ByteArray): String {
        val digest = MessageDigest.getInstance("SHA-256")
        listOf(domain.toByteArray(Charsets.UTF_8), signedMaterial, signature).forEach { value ->
            val length = value.size.toLong()
            repeat(Long.SIZE_BYTES) { index -> digest.update((length ushr (index * 8)).toByte()) }
            digest.update(value)
        }
        return Base64.encodeToString(
            digest.digest(),
            Base64.URL_SAFE or Base64.NO_WRAP or Base64.NO_PADDING,
        )
    }

    private const val TOKEN_OPERATION_TIMEOUT_MS = 8_000L
    private const val PREPARE_BACKOFF_MS = 15_000L
}
