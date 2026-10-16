package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.access.MagicianAccess
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.util.Log
import android.webkit.MimeTypeMap
import android.widget.Toast
import androidx.core.content.FileProvider
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.io.File
import java.io.FileOutputStream
import java.net.HttpURLConnection
import java.net.URL

/**
 * Opens and shares Magician-owned output bytes without putting credentials or
 * scope selectors in a URL. External URLs remain ordinary links; only URLs on
 * the enrolled Magician origin receive the device bearer.
 */
internal object AuthenticatedArtifacts {
    private const val TAG = "AuthenticatedArtifacts"
    private const val MAX_ARTIFACT_BYTES = 128L * 1024L * 1024L
    private const val MAX_CACHED_ARTIFACTS = 24

    suspend fun open(context: Context, url: String, filename: String? = null, mimeType: String? = null) {
        safely(context, "open") {
            openChecked(context, url, filename, mimeType)
        }
    }

    private suspend fun openChecked(context: Context, url: String, filename: String?, mimeType: String?) {
        if (!isMagicianUrl(context, url)) {
            withContext(Dispatchers.Main) {
                context.startActivity(
                    Intent(Intent.ACTION_VIEW, Uri.parse(url))
                        .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
                )
            }
            return
        }
        val artifact = download(context, url, filename, mimeType)
        withContext(Dispatchers.Main) {
            context.startActivity(
                Intent(Intent.ACTION_VIEW).apply {
                    setDataAndType(artifact.uri, artifact.mimeType)
                    addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_GRANT_READ_URI_PERMISSION)
                },
            )
        }
    }

    suspend fun share(context: Context, url: String, filename: String? = null, mimeType: String? = null) {
        safely(context, "share") {
            shareChecked(context, url, filename, mimeType)
        }
    }

    private suspend fun shareChecked(context: Context, url: String, filename: String?, mimeType: String?) {
        if (!isMagicianUrl(context, url)) {
            withContext(Dispatchers.Main) {
                context.startActivity(
                    Intent.createChooser(
                        Intent(Intent.ACTION_SEND).apply {
                            type = "text/plain"
                            putExtra(Intent.EXTRA_TEXT, url)
                            filename?.takeIf(String::isNotBlank)?.let {
                                putExtra(Intent.EXTRA_SUBJECT, it)
                            }
                        },
                        null,
                    ).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
                )
            }
            return
        }
        val artifact = download(context, url, filename, mimeType)
        withContext(Dispatchers.Main) {
            context.startActivity(
                Intent.createChooser(
                    Intent(Intent.ACTION_SEND).apply {
                        type = artifact.mimeType
                        putExtra(Intent.EXTRA_STREAM, artifact.uri)
                        addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
                    },
                    null,
                ).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_GRANT_READ_URI_PERMISSION),
            )
        }
    }

    private data class CachedArtifact(val uri: Uri, val mimeType: String)

    private suspend fun download(
        context: Context,
        rawUrl: String,
        requestedFilename: String?,
        requestedMimeType: String?,
    ): CachedArtifact = withContext(Dispatchers.IO) {
        val connection = URL(rawUrl).openConnection() as HttpURLConnection
        try {
            connection.requestMethod = "GET"
            // A redirect to another origin must never inherit the bearer or
            // Cloudflare service-token headers from this authenticated fetch.
            connection.instanceFollowRedirects = false
            connection.connectTimeout = 15_000
            connection.readTimeout = 60_000
            MagicianAccess.headers(context).forEach(connection::setRequestProperty)
            val status = connection.responseCode
            if (status !in 200..299) {
                throw IllegalStateException("artifact download rejected ($status)")
            }
            val declaredLength = connection.contentLengthLong
            if (declaredLength > MAX_ARTIFACT_BYTES) {
                throw IllegalStateException("artifact is too large to open on this device")
            }

            val directory = File(context.cacheDir, "magician-artifacts").apply { mkdirs() }
            prune(directory)
            val filename = safeFilename(
                requestedFilename
                    ?: URL(rawUrl).path.substringAfterLast('/').takeIf(String::isNotBlank)
                    ?: "artifact",
            )
            val destination = File(directory, "${System.currentTimeMillis()}-$filename")
            val partial = File(directory, ".${destination.name}.part")
            try {
                connection.inputStream.use { input ->
                    FileOutputStream(partial).use { output ->
                        val buffer = ByteArray(DEFAULT_BUFFER_SIZE)
                        var copied = 0L
                        while (true) {
                            val read = input.read(buffer)
                            if (read < 0) break
                            copied += read
                            if (copied > MAX_ARTIFACT_BYTES) {
                                throw IllegalStateException("artifact is too large to open on this device")
                            }
                            output.write(buffer, 0, read)
                        }
                    }
                }
                check(partial.renameTo(destination)) { "could not finalize artifact download" }
            } catch (error: Throwable) {
                partial.delete()
                throw error
            }

            val mimeType = requestedMimeType
                ?.substringBefore(';')
                ?.trim()
                ?.takeIf(String::isNotEmpty)
                ?: connection.contentType?.substringBefore(';')?.trim()?.takeIf(String::isNotEmpty)
                ?: MimeTypeMap.getSingleton()
                    .getMimeTypeFromExtension(destination.extension.lowercase())
                ?: "application/octet-stream"
            CachedArtifact(
                uri = FileProvider.getUriForFile(
                    context,
                    "${context.packageName}.artifact-files",
                    destination,
                ),
                mimeType = mimeType,
            )
        } finally {
            connection.disconnect()
        }
    }

    private fun isMagicianUrl(context: Context, rawUrl: String): Boolean {
        val base = runCatching { URL(MagicianAccess.baseUrl(context)) }.getOrNull() ?: return false
        val candidate = runCatching { URL(rawUrl) }.getOrNull() ?: return false
        return candidate.protocol.equals(base.protocol, ignoreCase = true)
            && candidate.host.equals(base.host, ignoreCase = true)
            && effectivePort(candidate) == effectivePort(base)
            && candidate.path.startsWith("/api/magician/")
    }

    private fun effectivePort(url: URL): Int =
        if (url.port >= 0) url.port else url.defaultPort

    private suspend fun safely(
        context: Context,
        action: String,
        operation: suspend () -> Unit,
    ) {
        try {
            operation()
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (error: Exception) {
            Log.w(TAG, "Could not $action artifact", error)
            withContext(Dispatchers.Main) {
                Toast.makeText(
                    context,
                    "Could not $action this output.",
                    Toast.LENGTH_SHORT,
                ).show()
            }
        }
    }

    private fun safeFilename(raw: String): String = raw
        .substringAfterLast('/')
        .replace(Regex("[^A-Za-z0-9._ -]"), "_")
        .trim('.', ' ')
        .take(120)
        .ifBlank { "artifact" }

    private fun prune(directory: File) {
        directory.listFiles()
            ?.filter(File::isFile)
            ?.sortedByDescending(File::lastModified)
            ?.drop(MAX_CACHED_ARTIFACTS - 1)
            ?.forEach(File::delete)
    }
}
