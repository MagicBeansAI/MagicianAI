package ai.magicbeans.magdroid.net

import android.content.Context
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import java.io.IOException
import java.net.ConnectException
import java.net.NoRouteToHostException
import java.net.SocketTimeoutException
import java.net.UnknownHostException
import java.security.cert.CertificateException
import javax.net.ssl.SSLException

/**
 * Why something could not be read, in words worth putting on a screen.
 *
 * Before this each repository wrote its own sentence and pasted the status code
 * into it — "Magician answered 502 for what needs you." Three things were wrong
 * with that. It names a number rather than a cause, so the one question the
 * owner actually has (is it my phone, my host, or the server?) goes unanswered.
 * It reads the same whether the fix is "wait", "start Magician" or "fix your
 * credentials". And because it was only ever a String, the screen had nothing to
 * decide a Retry button from, so most screens offered none.
 *
 * Classification lives here rather than in each screen so that one host being
 * down reads identically on every surface.
 */
data class Failure(
    val kind: FailureKind,
    /** One short line naming the cause. */
    val headline: String,
    /** What to do about it, and the status code for whoever wants it. */
    val detail: String,
    /** Whether trying the same thing again could plausibly work. */
    val retryable: Boolean = true,
    /** Whether the fix is in Settings rather than in time. */
    val setupRequired: Boolean = false,
)

enum class FailureKind {
    /** The phone has no connection. Nothing to do with Magician. */
    Offline,

    /** The phone is online; nothing answered at the host. */
    Unreachable,

    /** Magician answered, and the answer was that it broke. */
    ServerFault,

    /** Credentials were rejected. */
    Auth,

    /** The host has no such route — usually an older Magician. */
    NotFound,

    /** Something answered, but not something we could parse. */
    Garbled,

    Unknown,
}

/**
 * Turning a thrown thing, or a status code, into a [Failure].
 *
 * [doing] is a lowercase noun phrase naming what was being loaded — "what needs
 * you", "your maps", "this meeting" — so the detail line can say which read
 * failed without every call site writing its own sentence.
 */
object Failures {

    /**
     * A gateway that answers for an origin which did not. Cloudflare in front
     * of a stopped Magician produces exactly this, and it is the single most
     * common failure during development — so it is reported as "can't reach"
     * rather than as a server fault, because that is what it is.
     */
    private val GATEWAY = setOf(502, 503, 504)

    fun of(cause: Throwable, doing: String, online: Boolean = true): Failure {
        if (!online) return offline()
        return when (cause) {
            is UnknownHostException -> Failure(
                FailureKind.Unreachable,
                "Can't find your Magician host",
                "The host name did not resolve. Check the address in Settings.",
                setupRequired = true,
            )

            is SocketTimeoutException -> Failure(
                FailureKind.Unreachable,
                "Magician didn't answer in time",
                "The connection opened but nothing came back while loading $doing.",
            )

            is SSLException, is CertificateException -> Failure(
                FailureKind.Unreachable,
                "The secure connection failed",
                "Magician's certificate could not be trusted. Check the host in Settings.",
                setupRequired = true,
            )

            is ConnectException, is NoRouteToHostException -> unreachable(doing)
            is IOException -> unreachable(doing)
            else -> Failure(
                FailureKind.Unknown,
                "Something went wrong",
                cause.message ?: "Loading $doing failed.",
            )
        }
    }

    /** A response arrived and said no. */
    fun ofStatus(status: Int, doing: String): Failure = when {
        status in GATEWAY -> Failure(
            FailureKind.Unreachable,
            "Can't reach Magician",
            "The tunnel answered but Magician did not (HTTP $status). " +
                "Check that Magician is running, then try again.",
        )

        status == 401 || status == 403 -> Failure(
            FailureKind.Auth,
            "Magician refused the connection",
            "Your credentials were rejected (HTTP $status). " +
                "Check the host and Cloudflare Access details in Settings.",
            retryable = false,
            setupRequired = true,
        )

        status == 404 -> Failure(
            FailureKind.NotFound,
            "Not available on this Magician",
            "This host has no route for $doing (HTTP 404). It may be an older version.",
            retryable = false,
        )

        status >= 500 -> Failure(
            FailureKind.ServerFault,
            "Magician hit an error",
            "It failed while loading $doing (HTTP $status).",
        )

        else -> Failure(
            FailureKind.Unknown,
            "Magician couldn't answer",
            "It returned HTTP $status while loading $doing.",
        )
    }

    /** Something answered; it just wasn't readable. */
    fun garbled(doing: String): Failure = Failure(
        FailureKind.Garbled,
        "Magician sent something unexpected",
        "The reply could not be read while loading $doing.",
    )

    fun offline(): Failure = Failure(
        FailureKind.Offline,
        "You're offline",
        "This phone has no network connection. Reconnect and try again.",
    )

    private fun unreachable(doing: String) = Failure(
        FailureKind.Unreachable,
        "Can't reach Magician",
        "Nothing answered at your Magician host while loading $doing. " +
            "Check that it is running, then try again.",
    )
}

/**
 * An error that already knows why it happened.
 *
 * A repository that read a status code knows more than the ViewModel catching
 * it ever can — 401 against 502 is the difference between "fix your
 * credentials" and "start the server". Implementing this lets that knowledge
 * survive the throw instead of being flattened into a sentence.
 */
interface CarriesFailure {
    val failure: Failure
}

/**
 * The one call every ViewModel makes in its `onFailure`.
 *
 * Prefers what the thrower already classified, and otherwise classifies the
 * transport exception here — where a Context is available to tell "this phone
 * is offline" apart from "that host is down", which is the distinction the
 * repository could not have drawn on its own.
 */
fun Throwable.toFailure(context: Context, doing: String): Failure =
    (this as? CarriesFailure)?.failure
        ?: Failures.of(this, doing, Connectivity.isOnline(context))

/**
 * Whether this phone currently has a validated connection.
 *
 * Kept apart from the classifier so the classifier stays a pure function and
 * can be tested without a device. `VALIDATED` rather than merely connected: a
 * captive-portal Wi-Fi is attached to a network and still cannot reach a thing,
 * and reporting that as "Magician is down" sends somebody to restart a server
 * that was never the problem.
 */
object Connectivity {
    fun isOnline(context: Context): Boolean {
        val manager = context.getSystemService(Context.CONNECTIVITY_SERVICE) as? ConnectivityManager
            ?: return true
        val active = manager.activeNetwork ?: return false
        val caps = manager.getNetworkCapabilities(active) ?: return false
        return caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET) &&
            caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED)
    }
}
