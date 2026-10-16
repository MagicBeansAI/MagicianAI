package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.access.DeviceEnrollmentLink
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.lifecycle.ViewModel

/** A scanner exit that needs to be explained by the App Pilot surface. */
internal enum class PairingScanIssue {
    Cancelled,
    CameraPermissionDenied,
}

/** Keeps the third-party scanner result mapping small, explicit, and testable. */
internal data class PairingScanResolution(
    val enrollmentUri: String? = null,
    val issue: PairingScanIssue? = null,
)

/** Activity-owned scanner state and actions consumed by a Compose pairing surface. */
internal data class PairingUiBridge(
    val pendingEnrollmentUri: String? = null,
    val onEnrollmentConsumed: () -> Unit = {},
    val scanIssue: PairingScanIssue? = null,
    val onScanIssueConsumed: () -> Unit = {},
    val onScan: () -> Unit = {},
    val onPaired: () -> Unit = {},
)

internal fun resolvePairingScan(
    contents: String?,
    missingCameraPermission: Boolean,
): PairingScanResolution {
    val enrollmentUri = contents?.trim()?.takeIf(String::isNotEmpty)
    return when {
        enrollmentUri != null -> PairingScanResolution(enrollmentUri = enrollmentUri)
        missingCameraPermission -> PairingScanResolution(issue = PairingScanIssue.CameraPermissionDenied)
        else -> PairingScanResolution(issue = PairingScanIssue.Cancelled)
    }
}

/** A different QR cannot replace a possibly submitted App Pilot exchange. */
internal fun shouldResumeRetainedAppsEnrollment(
    scanned: DeviceEnrollmentLink,
    retained: DeviceEnrollmentLink?,
): Boolean = retained != null &&
    (retained.baseUrl != scanned.baseUrl || retained.enrollmentId != scanned.enrollmentId)

/** Keeps a scanned one-time capability across rotation without persisting its secret. */
internal class PairingStateViewModel : ViewModel() {
    var pendingEnrollmentUri by mutableStateOf<String?>(null)
    var pendingScanIssue by mutableStateOf<PairingScanIssue?>(null)
}
