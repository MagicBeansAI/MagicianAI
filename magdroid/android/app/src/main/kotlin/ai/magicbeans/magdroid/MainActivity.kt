package ai.magicbeans.magdroid

import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.viewModels
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import ai.magicbeans.magdroid.ui.AppPilotScreen
import ai.magicbeans.magdroid.ui.BindWindowTheme
import ai.magicbeans.magdroid.ui.PairingScanIssue
import ai.magicbeans.magdroid.ui.PairingStateViewModel
import ai.magicbeans.magdroid.ui.MagicanTheme
import ai.magicbeans.magdroid.ui.ThemeStore
import ai.magicbeans.magdroid.ui.Themes
import ai.magicbeans.magdroid.ui.applyTheme
import ai.magicbeans.magdroid.ui.resolvePairingScan
import com.google.zxing.client.android.Intents
import com.journeyapps.barcodescanner.ScanContract
import com.journeyapps.barcodescanner.ScanOptions

/**
 * Standalone entry point for App Pilot.
 *
 * The class name is retained because existing intents and developer tooling
 * already open it. The old XML page has been replaced by the same Compose and
 * theme pipeline as the rest of Magican. Status contains the complete operational
 * and setup contract in state-aware accordions; Activity stays separate.
 */
class MainActivity : ComponentActivity() {
    private val pairingState by viewModels<PairingStateViewModel>()

    private val qrScanner = registerForActivityResult(ScanContract()) { result ->
        val resolution = resolvePairingScan(
            contents = result.contents,
            missingCameraPermission = result.originalIntent
                ?.getBooleanExtra(Intents.Scan.MISSING_CAMERA_PERMISSION, false) == true,
        )
        pairingState.pendingEnrollmentUri = resolution.enrollmentUri
        pairingState.pendingScanIssue = resolution.issue
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        acceptEnrollmentIntent(intent)
        setContent {
            val store = remember { ThemeStore.get(this@MainActivity) }
            val systemIsDark = isSystemInDarkTheme()
            val familyId by store.familyId.collectAsStateWithLifecycle()
            val mode by store.mode.collectAsStateWithLifecycle()
            val themeId = Themes.resolve(familyId, mode, systemIsDark)
            val palette = Themes.palette(themeId)
            LaunchedEffect(familyId, mode, systemIsDark) {
                applyTheme(familyId, mode, systemIsDark)
            }
            MagicanTheme(themeId, palette) {
                BindWindowTheme(this@MainActivity, palette)
                AppPilotScreen(
                    onBack = ::finish,
                    pendingEnrollmentUri = pairingState.pendingEnrollmentUri,
                    onEnrollmentConsumed = { pairingState.pendingEnrollmentUri = null },
                    pairingScanIssue = pairingState.pendingScanIssue,
                    onPairingScanIssueConsumed = { pairingState.pendingScanIssue = null },
                    onScanPairing = {
                        qrScanner.launch(
                            ScanOptions()
                                .setDesiredBarcodeFormats(ScanOptions.QR_CODE)
                                .setPrompt("Scan the pairing code shown by Magician")
                                .setBeepEnabled(false)
                                .setOrientationLocked(false)
                                .addExtra(Intents.Scan.SHOW_MISSING_CAMERA_PERMISSION_DIALOG, false),
                        )
                    },
                )
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        acceptEnrollmentIntent(intent)
    }

    private fun acceptEnrollmentIntent(source: Intent?) {
        source?.dataString
            ?.takeIf { source.action == Intent.ACTION_VIEW }
            ?.let {
                pairingState.pendingEnrollmentUri = it
                pairingState.pendingScanIssue = null
                // Do not reopen a consumed deep link after Activity recreation.
                source.action = null
                source.data = null
            }
    }
}
