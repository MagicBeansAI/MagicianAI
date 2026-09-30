package ai.magicbeans.magdroid.ui

import android.content.Intent
import android.os.Bundle
import android.os.Build
import android.graphics.drawable.ColorDrawable
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.DisposableEffect
import androidx.activity.ComponentActivity
import androidx.activity.viewModels
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.activity.compose.setContent
import androidx.compose.ui.graphics.toArgb
import androidx.core.view.WindowCompat
import ai.magicbeans.magdroid.push.AndroidMobilePushRegistration
import com.google.zxing.client.android.Intents
import com.journeyapps.barcodescanner.ScanContract
import com.journeyapps.barcodescanner.ScanOptions

/**
 * The chat surface.
 *
 * A `ComponentActivity` hosting Compose: a streaming reply changes its bubble
 * many times a second, and a declarative tree redrawing from state is the right
 * shape for that. App Pilot uses the same host and palette pipeline, so moving
 * between the two cannot change the selected Magican theme.
 */
class ChatActivity : ComponentActivity() {
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
        acceptMobileEnrollmentIntent(intent)
        // A task link and a launcher shortcut arrive the same way and mean
        // different things, so both are offered the URI and each takes what it
        // recognises.
        TaskDeepLinks.accept(intent?.data)
        AppShortcutLinks.accept(intent?.data)
        if (intent?.data?.host == "attention") {
            AttentionDeepLinks.request(
                intent.data?.getQueryParameter("item")
                    ?: intent.data?.getQueryParameter("correlation_id")
            )
        }
        AndroidMobilePushRegistration.ensure(this)
        setContent {
            // Resolve Material and Magican tokens from the same observed choice.
            // Material previously stayed permanently on its light defaults.
            val store = remember { ThemeStore.get(this@ChatActivity) }
            val systemIsDark = isSystemInDarkTheme()
            val familyId by store.familyId.collectAsStateWithLifecycle()
            val mode by store.mode.collectAsStateWithLifecycle()
            val themeId = Themes.resolve(familyId, mode, systemIsDark)
            val palette = Themes.palette(themeId)
            LaunchedEffect(familyId, mode, systemIsDark) {
                applyTheme(familyId, mode, systemIsDark)
            }
            MagicanTheme(themeId, palette) {
                // The app palette owns the Android chrome too. XML can only
                // provide the launch colour; it cannot follow a family or an
                // in-app Day/Night override after the activity is visible.
                BindWindowTheme(this@ChatActivity, palette)

                val viewModel: ai.magicbeans.magdroid.chat.ChatViewModel =
                    androidx.lifecycle.viewmodel.compose.viewModel()
                // A screenshot shared into the app opens the board over
                // everything else. Taken once, so returning to the app later
                // does not replay a lesson that was already given.
                var lesson by remember { mutableStateOf(TutorShareInbox.take()) }
                // Anything shared to the plain target lands in the composer:
                // the text becomes the draft, the files become staged
                // attachments. Taken once, for the same reason as the lesson
                // above — coming back to the app should not re-attach what was
                // already attached.
                LaunchedEffect(Unit) {
                    ChatShareInbox.take()?.let { shared ->
                        if (shared.text.isNotBlank()) viewModel.onDraftChange(shared.text)
                        shared.files.forEach { file ->
                            viewModel.stageAttachment(file.name, file.mime, file.bytes)
                        }
                    }
                }
                // A lesson started from chat has no shared picture, so the
                // board is the same surface with nothing behind the drawing.
                val chatLesson by viewModel.tutorRun.collectAsStateWithLifecycle()
                val pending = lesson ?: chatLesson?.let {
                    TutorShareInbox.Pending(
                        image = null,
                        question = "",
                        surface = ai.magicbeans.magdroid.tutor.TutorSurface.Blackboard,
                    )
                }
                if (pending != null) {
                    // One run per lesson, kept across recomposition so the
                    // shapes it has accumulated are not dropped every frame.
                    // The run the chat turn started, or a fresh one for a
                    // shared screenshot that has not been asked about yet.
                    val run = chatLesson ?: remember(pending) { ai.magicbeans.magdroid.tutor.TutorRun() }
                    val shapes by run.shapesFlow.collectAsStateWithLifecycle()
                    val spoken by run.captionFlow.collectAsStateWithLifecycle()
                    val progress = rememberStoryboardProgress(shapes)

                    // A lesson about another app is drawn over that app, not
                    // inside this one. The board would be the wrong surface —
                    // by the time it is on screen, YouTube is behind it and the
                    // thing being explained is gone.
                    val overOtherApps = pending.surface ==
                        ai.magicbeans.magdroid.tutor.TutorSurface.Overlay
                    LaunchedEffect(overOtherApps, shapes) {
                        if (overOtherApps) TutorOverlayService.show(this@ChatActivity, shapes)
                    }
                    DisposableEffect(overOtherApps) {
                        onDispose {
                            if (overOtherApps) TutorOverlayService.hide(this@ChatActivity)
                        }
                    }
                    // Nothing of ours on screen while the overlay is up: this
                    // activity would cover the app the lesson is about.
                    if (overOtherApps) {
                        LaunchedEffect(Unit) { moveTaskToBack(true) }
                    } else {
                        TutorBlackboard(
                        shapes = shapes,
                        progress = progress,
                        // The question until the tutor says something of its
                        // own, so the board is never captionless while it
                        // thinks.
                        caption = spoken ?: pending.question,
                        subject = pending.image,
                        onClose = {
                            lesson = null
                            viewModel.endLesson()
                        },
                    )
                    }
                } else AppShell(
                    viewModel = viewModel,
                    onOpenAppPilot = {
                        startActivity(
                            android.content.Intent(this, ai.magicbeans.magdroid.MainActivity::class.java)
                        )
                        // The app's own motion rather than the platform default.
                        // App Pilot is a forward step, which is exactly what the
                        // wave pair draws: in from the right, out to the left.
                        ai.magicbeans.magdroid.animation.WaveTransitions.applyWaveTransitions(
                            this,
                            ai.magicbeans.magdroid.R.anim.wave_enter,
                            ai.magicbeans.magdroid.R.anim.wave_exit,
                        )
                    },
                    mobilePairing = PairingUiBridge(
                        onPaired = viewModel::onConnectionChanged,
                        pendingEnrollmentUri = pairingState.pendingEnrollmentUri,
                        onEnrollmentConsumed = { pairingState.pendingEnrollmentUri = null },
                        scanIssue = pairingState.pendingScanIssue,
                        onScanIssueConsumed = { pairingState.pendingScanIssue = null },
                        onScan = {
                            qrScanner.launch(
                                ScanOptions()
                                    .setDesiredBarcodeFormats(ScanOptions.QR_CODE)
                                    .setPrompt("Scan Connect Android from Magican Desktop")
                                    .setBeepEnabled(false)
                                    .setOrientationLocked(false)
                                    .addExtra(Intents.Scan.SHOW_MISSING_CAMERA_PERMISSION_DIALOG, false),
                            )
                        },
                    ),
                )
            }
        }
    }

    override fun onNewIntent(intent: android.content.Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        TaskDeepLinks.accept(intent.data)
        AppShortcutLinks.accept(intent.data)
        acceptMobileEnrollmentIntent(intent)
        if (intent.data?.host == "attention") {
            AttentionDeepLinks.request(
                intent.data?.getQueryParameter("item")
                    ?: intent.data?.getQueryParameter("correlation_id")
            )
        }
    }

    private fun acceptMobileEnrollmentIntent(source: Intent?) {
        source?.dataString
            ?.takeIf {
                source.action == Intent.ACTION_VIEW &&
                    source.data?.host in setOf("connect", "pair")
            }
            ?.let {
                pairingState.pendingEnrollmentUri = it
                pairingState.pendingScanIssue = null
                source.action = null
                source.data = null
            }
    }
}

@androidx.compose.runtime.Composable
internal fun BindWindowTheme(activity: ComponentActivity, palette: Palette) {
    SideEffect {
        val color = palette.background.toArgb()
        // The native window remains visible behind transparent content and
        // during transitions; it must not retain the XML launch palette.
        activity.window.setBackgroundDrawable(ColorDrawable(color))
        activity.window.statusBarColor = color
        activity.window.navigationBarColor = color
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            activity.window.navigationBarDividerColor = color
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            // Android's automatic contrast scrim otherwise changes the chosen
            // palette behind three-button navigation.
            activity.window.isNavigationBarContrastEnforced = false
            activity.window.isStatusBarContrastEnforced = false
        }
        WindowCompat.getInsetsController(activity.window, activity.window.decorView).apply {
            isAppearanceLightStatusBars = !palette.isDark
            isAppearanceLightNavigationBars = !palette.isDark
        }
    }
}
