package ai.magicbeans.magdroid.mcp

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.util.Base64
import android.util.DisplayMetrics
import android.util.Log
import android.view.WindowManager
import ai.magicbeans.magdroid.gesture.GestureEngine
import ai.magicbeans.magdroid.input.InputEngine
import ai.magicbeans.magdroid.notification.ChallengeDeposit
import ai.magicbeans.magdroid.notification.NotificationListener
import ai.magicbeans.magdroid.notification.OtpWatcher
import ai.magicbeans.magdroid.protection.ProtectedApps
import ai.magicbeans.magdroid.protection.ProtectionGate
import ai.magicbeans.magdroid.protection.VerificationTextScrub
import ai.magicbeans.magdroid.screenshot.ScreenshotPipeline
import ai.magicbeans.magdroid.service.AccessibilityEventListener
import ai.magicbeans.magdroid.service.GestureResultCallback
import ai.magicbeans.magdroid.service.MagdroidAccessibilityService
import ai.magicbeans.magdroid.service.ScreenshotQuality
import ai.magicbeans.magdroid.service.Bounds
import ai.magicbeans.magdroid.service.UiElement
import ai.magicbeans.magdroid.service.UiTree
import ai.magicbeans.magdroid.uitree.UiTreeWalker
import kotlinx.coroutines.*
import kotlinx.serialization.json.*
import android.accessibilityservice.GestureDescription
import kotlin.coroutines.resume
import java.io.ByteArrayOutputStream
import java.security.MessageDigest

internal enum class AppsAccessibilityWindowKind {
    Application,
    NonApplication,
    Unknown,
}

/** Pure fail-closed classifier used by the live Apps physical owner and tests. */
internal fun appsProtectedSystemSurfaceKind(
    foregroundPackage: String,
    launcherPackage: String?,
    windowKind: AppsAccessibilityWindowKind,
): String? {
    if (foregroundPackage == "com.android.systemui") return "system_ui"
    if (launcherPackage != null && foregroundPackage == launcherPackage) {
        return "launcher_or_recents"
    }
    return when (windowKind) {
        AppsAccessibilityWindowKind.Application -> null
        AppsAccessibilityWindowKind.NonApplication -> "system_window"
        AppsAccessibilityWindowKind.Unknown -> "unknown_window"
    }
}

class McpToolHandler(
    private val service: MagdroidAccessibilityService,
    private val gestureEngine: GestureEngine,
    private val uiTreeWalker: UiTreeWalker,
    private val inputEngine: InputEngine,
    private val screenshotPipeline: ScreenshotPipeline,
    private val protectedApps: ProtectedApps
) {
    companion object {
        private const val TAG = "McpToolHandler"

        /** Grown slightly so an edge pixel of a digit cannot survive. */
        private const val REDACTION_MARGIN_PX = 3f
        private const val DEFAULT_TIMEOUT_MS = 10000L
        private const val POLL_INTERVAL_MS = 300L

        /** Stands in for a notification that carries a verification code. */
        private const val CODE_WITHHELD = VerificationTextScrub.WITHHELD

        // Server-side ceilings, regardless of what the tool schema documents.
        // An uncapped caller-chosen duration is how a tool that passed the
        // protection gate at entry keeps running long after the gate would
        // refuse it.
        private const val MAX_WAIT_TIMEOUT_MS = 30_000L
        private const val MAX_SCROLL_ATTEMPTS = 30
        private const val MAX_GESTURE_DURATION_MS = 5_000L
    }

    private data class AppsStructuredSnapshot(
        val foregroundPackage: String,
        val width: Int,
        val height: Int,
        val table: String,
        val totalNodes: Int,
        val shownNodes: Int,
        val truncated: Boolean,
        val foreignPackageDetected: Boolean,
        val snapshotSha256: String
    )

    private suspend fun appsStructuredSnapshot(
        rootNode: android.view.accessibility.AccessibilityNodeInfo,
        expectedPackage: String,
        maxDepth: Int,
        maxNodes: Int,
        maxResultBytes: Int
    ): AppsStructuredSnapshot {
        // Reserve a fixed envelope allowance before the walker allocates any
        // table bytes. The final exact JSON byte check below remains the last
        // word, but can no longer discover a multi-megabyte intermediate.
        val tableBudget = (maxResultBytes - 4096).coerceAtLeast(0)
        val metrics = service.resources.displayMetrics
        val projection = uiTreeWalker.walkAppsProjection(
            rootNode = rootNode,
            expectedPackage = expectedPackage,
            displayWidth = metrics.widthPixels,
            displayHeight = metrics.heightPixels,
            maxDepth = maxDepth,
            maxNodes = maxNodes,
            maxTableBytes = tableBudget
        )
        val table = projection.table
        val digestMaterial = buildString {
            append(APPS_OWNER_SCHEMA)
            append('\n')
            append(expectedPackage)
            append('\n')
            append(metrics.widthPixels)
            append('x')
            append(metrics.heightPixels)
            append('\n')
            append(projection.truncated)
            append('\n')
            append(table)
        }
        val digest = MessageDigest.getInstance("SHA-256")
            .digest(digestMaterial.toByteArray(Charsets.UTF_8))
            .joinToString("") { "%02x".format(it.toInt() and 0xff) }
        return AppsStructuredSnapshot(
            foregroundPackage = expectedPackage,
            width = metrics.widthPixels,
            height = metrics.heightPixels,
            table = table,
            totalNodes = projection.totalNodes,
            shownNodes = projection.shownNodes,
            truncated = projection.truncated,
            foreignPackageDetected = projection.foreignPackageDetected,
            snapshotSha256 = digest
        )
    }

    suspend fun handleToolCall(toolName: String, arguments: JsonObject?): McpToolCallResult {
        val args = arguments ?: JsonObject(emptyMap())
        return try {
            // Parsing happens before dispatch. A private claim on another tool,
            // an action/tool mismatch, or one ambient legacy option cannot fall
            // through to the broader MCP handler.
            val ownerClaim = parseAppsOwnerClaim(toolName, args)

            // The protection gate, at the one seam every tool passes through.
            // Observe/act tools are refused while a protected app is foregrounded;
            // the notification tools filter per entry inside their handlers, where
            // the source package is known. This entry check is NOT the whole gate:
            // the tree-reading handlers re-check against the root they actually
            // walk, and the polling handlers re-check every iteration — a tool
            // that was gated when it started can outlive a foreground change.
            val foreground = activeForegroundPackage()
            if (
                ProtectionGate.classify(toolName) == ProtectionGate.Disposition.FOREGROUND_GATED &&
                protectedApps.isProtected(foreground) &&
                !isNavigationEscape(toolName, args)
            ) {
                return ProtectionGate.refusal(foreground!!)
            }

            (if (ownerClaim != null) {
                handleAppsOwnerTool(toolName, args, ownerClaim)
            } else when (toolName) {
                // OBSERVE
                "android_get_ui_tree" -> handleGetUiTree(args)
                "android_screenshot" -> handleScreenshot(args)
                "android_find_elements" -> handleFindElements(args)
                "android_get_screen_context" -> handleGetScreenContext(args)
                "android_get_notifications" -> handleGetNotifications(args)
            "android_await_otp" -> handleAwaitOtp(args)
                "android_accessibility_audit" -> handleAccessibilityAudit(args)
                "android_screenshot_diff" -> handleScreenshotDiff(args)
                "android_get_recent_toasts" -> handleGetRecentToasts(args)

                // ACT
                "android_tap" -> handleTap(args)
                "android_long_press" -> handleLongPress(args)
                "android_double_tap" -> handleDoubleTap(args)
                "android_swipe" -> handleSwipe(args)
                "android_pinch" -> handlePinch(args)
                "android_drag" -> handleDrag(args)
                "android_input_text" -> handleInputText(args)
                "android_press_key" -> handlePressKey(args)
                "android_global_action" -> handleGlobalAction(args)

                // MANAGE
                "android_launch_app" -> handleLaunchApp(args)
                "android_close_app" -> handleCloseApp(args)
                "android_open_url" -> handleOpenUrl(args)
                "android_set_clipboard" -> handleSetClipboard(args)
                "android_list_apps" -> handleListApps(args)

                // WAIT
                "android_wait_for_element" -> handleWaitForElement(args)
                "android_wait_for_gone" -> handleWaitForGone(args)
                "android_wait_for_idle" -> handleWaitForIdle(args)
                "android_scroll_to_element" -> handleScrollToElement(args)

                // DEVICE (stubs — we are the device)
                "android_list_devices" -> handleListDevices()
                "android_select_device" -> textResult("{\"success\":true,\"message\":\"Already connected to this device\"}")

                // META
                "android_search_tools" -> handleSearchTools(args)
                "android_describe_tools" -> handleDescribeTools(args)

                // TEST
                "android_enable_events" -> handleEnableEvents(args)
                "android_get_device_info" -> handleGetDeviceInfo()

                else -> errorResult("Unknown tool: $toolName")
            }).withVerificationTextWithheld(toolName).withForegroundPackage(foreground)
        } catch (e: Exception) {
            Log.e(TAG, "Tool error: $toolName", e)
            errorResult("Tool error: ${e.message}")
        }
    }

    /** Execute only the exact private eight-tool Apps owner surface. */
    private suspend fun handleAppsOwnerTool(
        toolName: String,
        args: JsonObject,
        claim: AppsOwnerClaim,
    ): McpToolCallResult = when (toolName) {
        "android_get_ui_tree" -> handleAppsOwnerSnapshot(claim)
        "android_screenshot" -> handleAppsOwnerScreenshot(claim)
        "android_launch_app" -> handleAppsOwnerLaunch(claim)
        "android_close_app" -> handleAppsOwnerClose(claim)
        "android_tap" -> handleAppsOwnerTap(args, claim)
        "android_input_text" -> handleAppsOwnerType(args, claim)
        "android_press_key" -> handleAppsOwnerKey(args, claim)
        "android_swipe" -> handleAppsOwnerScroll(args, claim)
        else -> throw IllegalArgumentException("Apps owner claim is not valid for this tool")
    }

    private suspend fun handleAppsOwnerSnapshot(claim: AppsOwnerClaim): McpToolCallResult {
        val snapshot = captureAppsStructuredSnapshot(claim, expectedPackage = null)
        val text = buildJsonObject {
            put("format", "apps_compact_v1")
            put("app", snapshot.foregroundPackage)
            put("width", snapshot.width)
            put("height", snapshot.height)
            put("total", snapshot.totalNodes)
            put("shown", snapshot.shownNodes)
            put("truncated", snapshot.truncated)
            put("snapshot_sha256", snapshot.snapshotSha256)
            put("elements", snapshot.table)
        }.toString()
        return appsOwnerTextResult(
            claim = claim,
            text = text,
            foregroundPackage = snapshot.foregroundPackage,
            snapshotSha256 = snapshot.snapshotSha256,
            width = snapshot.width,
            height = snapshot.height,
            evidenceNodes = snapshot.shownNodes,
            truncated = snapshot.truncated,
        )
    }

    private suspend fun handleAppsOwnerScreenshot(claim: AppsOwnerClaim): McpToolCallResult {
        val target = requireNotNull(claim.targetPackage)
        val before = requireActiveAppsTarget(target)
        val regionsBefore = verificationRegionsOnScreen()
        val captured = screenshotPipeline.capture(ScreenshotQuality.FULL)
        val regionsAfter = verificationRegionsOnScreen()
        // A heads-up banner can carry a code over the target app, so this
        // capture is held the same way the open one is — before the ceiling is
        // measured, since holding changes the size. No marker field: this
        // result is decoded with deny-unknown-fields, and the black regions
        // are visible in the image itself.
        val jpegBytes = holdVerificationRegions(captured, regionsBefore, regionsAfter)?.bytes
            ?: throw IllegalStateException(
                "A verification code is on screen and this capture could not be held",
            )
        require(
            jpegBytes.isNotEmpty() &&
                jpegBytes.size <= claim.maxResultBytes &&
                jpegBytes.size <= MAX_APPS_OWNER_SCREENSHOT_BYTES,
        ) {
            "Apps screenshot exceeds the admitted result ceiling"
        }
        val after = requireActiveAppsTarget(target)
        require(before == after) { "Apps screenshot geometry changed during capture" }
        val metadata = buildJsonObject {
            put("width", after.first)
            put("height", after.second)
            put("format", "jpeg")
        }.toString()
        return appsOwnerResult(
            claim = claim,
            content = listOf(
                McpContentBlock(
                    type = "image",
                    data = Base64.encodeToString(jpegBytes, Base64.NO_WRAP),
                    mimeType = "image/jpeg",
                ),
                McpContentBlock(type = "text", text = metadata),
            ),
            exactPayload = jpegBytes,
            foregroundPackage = target,
            snapshotSha256 = null,
            width = after.first,
            height = after.second,
            evidenceNodes = 0,
            truncated = false,
        )
    }

    private suspend fun handleAppsOwnerLaunch(claim: AppsOwnerClaim): McpToolCallResult {
        val target = requireNotNull(claim.targetPackage)
        val launchIntent = service.packageManager.getLaunchIntentForPackage(target)
            ?: throw IllegalArgumentException("No launch intent found for reviewed package")
        launchIntent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        service.startActivity(launchIntent)
        require(waitForAppsForeground(target, shouldMatch = true)) {
            "Apps launch did not settle on the reviewed package"
        }
        val text = buildJsonObject {
            put("package_name", target)
            put("latency_ms", 0)
        }.toString()
        return appsOwnerTextResult(claim, text, foregroundPackage = target)
    }

    private suspend fun handleAppsOwnerClose(claim: AppsOwnerClaim): McpToolCallResult {
        val target = requireNotNull(claim.targetPackage)
        requireActiveAppsTarget(target)
        require(
            service.performGlobalAction(
                android.accessibilityservice.AccessibilityService.GLOBAL_ACTION_HOME,
            ),
        ) { "Apps close action was refused by Android" }
        require(waitForAppsForeground(target, shouldMatch = false)) {
            "Apps close did not leave the reviewed package"
        }
        val completion = activeForegroundPackage()?.takeIf { it in claim.allowedPackages }
        val text = buildJsonObject {
            put("package_name", target)
            put("closed", true)
        }.toString()
        return appsOwnerTextResult(claim, text, foregroundPackage = completion)
    }

    private suspend fun handleAppsOwnerTap(
        args: JsonObject,
        claim: AppsOwnerClaim,
    ): McpToolCallResult {
        revalidateAppsObservation(claim)
        val x = requireNotNull(args["x"]?.jsonPrimitive?.intOrNull).toFloat()
        val y = requireNotNull(args["y"]?.jsonPrimitive?.intOrNull).toFloat()
        val success = withTimeoutOrNull(5_000L) {
            executeGestureAndWait { callback -> gestureEngine.executeTap(x, y, callback) }
        } ?: false
        require(success) { "Apps tap gesture was cancelled or timed out" }
        verifyAppsMutationCompletion(claim, mayLeaveTarget = false)
        return appsOwnerTextResult(
            claim,
            "{\"latency_ms\":0}",
            foregroundPackage = claim.targetPackage,
            snapshotSha256 = claim.observation?.snapshotSha256,
        )
    }

    private suspend fun handleAppsOwnerType(
        args: JsonObject,
        claim: AppsOwnerClaim,
    ): McpToolCallResult {
        revalidateAppsObservation(claim)
        val text = requireNotNull(args["text"]?.jsonPrimitive?.contentOrNull)
        val focused = service.findFocus(android.view.accessibility.AccessibilityNodeInfo.FOCUS_INPUT)
            ?: throw IllegalArgumentException("Apps type requires a focused input")
        val success = try {
            inputEngine.inputText(focused, text, false)
        } finally {
            @Suppress("DEPRECATION")
            focused.recycle()
        }
        require(success) { "Apps type action failed" }
        verifyAppsMutationCompletion(claim, mayLeaveTarget = false)
        return appsOwnerTextResult(
            claim,
            "{\"latency_ms\":0,\"target\":\"focused\"}",
            foregroundPackage = claim.targetPackage,
            snapshotSha256 = claim.observation?.snapshotSha256,
        )
    }

    private suspend fun handleAppsOwnerKey(
        args: JsonObject,
        claim: AppsOwnerClaim,
    ): McpToolCallResult {
        revalidateAppsObservation(claim)
        val key = requireNotNull(args["key"]?.jsonPrimitive?.contentOrNull)
        val globalAction = when (key) {
            "back" -> android.accessibilityservice.AccessibilityService.GLOBAL_ACTION_BACK
            "home" -> android.accessibilityservice.AccessibilityService.GLOBAL_ACTION_HOME
            else -> null
        }
        val success = if (globalAction != null) {
            service.performGlobalAction(globalAction)
        } else {
            val root = service.rootInActiveWindow
            val focused = try {
                root?.findFocus(android.view.accessibility.AccessibilityNodeInfo.FOCUS_INPUT)
            } finally {
                @Suppress("DEPRECATION")
                root?.recycle()
            }
            try {
                inputEngine.pressKey(key, focused)
            } finally {
                @Suppress("DEPRECATION")
                focused?.recycle()
            }
        }
        require(success) { "Apps key action failed" }
        val mayLeaveTarget = key == "back" || key == "home"
        val completion = verifyAppsMutationCompletion(claim, mayLeaveTarget)
        val result = buildJsonObject {
            put("key", key)
            put("latency_ms", 0)
        }.toString()
        return appsOwnerTextResult(
            claim,
            result,
            foregroundPackage = completion,
            snapshotSha256 = claim.observation?.snapshotSha256,
        )
    }

    private suspend fun handleAppsOwnerScroll(
        args: JsonObject,
        claim: AppsOwnerClaim,
    ): McpToolCallResult {
        revalidateAppsObservation(claim)
        val startX = requireNotNull(args["start_x"]?.jsonPrimitive?.intOrNull).toFloat()
        val startY = requireNotNull(args["start_y"]?.jsonPrimitive?.intOrNull).toFloat()
        val endX = requireNotNull(args["end_x"]?.jsonPrimitive?.intOrNull).toFloat()
        val endY = requireNotNull(args["end_y"]?.jsonPrimitive?.intOrNull).toFloat()
        val durationMs = requireNotNull(args["duration_ms"]?.jsonPrimitive?.longOrNull)
        val success = withTimeoutOrNull(durationMs + 2_000L) {
            executeGestureAndWait { callback ->
                gestureEngine.executeSwipe(startX, startY, endX, endY, durationMs, callback)
            }
        } ?: false
        require(success) { "Apps scroll gesture was cancelled or timed out" }
        verifyAppsMutationCompletion(claim, mayLeaveTarget = false)
        return appsOwnerTextResult(
            claim,
            "{\"latency_ms\":0}",
            foregroundPackage = claim.targetPackage,
            snapshotSha256 = claim.observation?.snapshotSha256,
        )
    }

    private suspend fun revalidateAppsObservation(claim: AppsOwnerClaim) {
        val expected = requireNotNull(claim.observation)
        val snapshot = captureAppsStructuredSnapshot(
            claim,
            expectedPackage = requireNotNull(claim.targetPackage),
            maxNodes = MAX_APPS_OWNER_NODES,
            maxResultBytes = 1024 * 1024,
        )
        require(
            snapshot.foregroundPackage == expected.foregroundPackage &&
                snapshot.snapshotSha256 == expected.snapshotSha256 &&
                snapshot.width == expected.width &&
                snapshot.height == expected.height,
        ) { "Apps observation is stale" }
    }

    private suspend fun captureAppsStructuredSnapshot(
        claim: AppsOwnerClaim,
        expectedPackage: String?,
        maxNodes: Int = claim.maxEvidenceNodes,
        maxResultBytes: Int = claim.maxResultBytes,
    ): AppsStructuredSnapshot {
        val root = service.rootInActiveWindow
            ?: throw IllegalArgumentException("Apps snapshot has no active window")
        return try {
            val foreground = root.packageName?.toString()
                ?: throw IllegalArgumentException("Apps snapshot has no attributable package")
            require(expectedPackage == null || foreground == expectedPackage) {
                "Apps foreground package does not match target"
            }
            require(foreground in claim.allowedPackages) {
                "Apps foreground package is outside the reviewed set"
            }
            require(protectedSystemSurface(root, foreground) == null) {
                "Apps owner refuses protected system surfaces"
            }
            require(!protectedApps.isProtected(foreground)) {
                "Apps owner refuses a protected app"
            }
            val initialMetrics = service.resources.displayMetrics
            val initialWidth = initialMetrics.widthPixels
            val initialHeight = initialMetrics.heightPixels
            val snapshot = appsStructuredSnapshot(
                rootNode = root,
                expectedPackage = foreground,
                maxDepth = 50,
                maxNodes = maxNodes,
                maxResultBytes = maxResultBytes,
            )
            val completion = activeAppsSurface()
            val completionMetrics = service.resources.displayMetrics
            require(
                completion.refusal == null &&
                    completion.packageName == foreground &&
                    !snapshot.foreignPackageDetected &&
                    !protectedApps.isProtected(completion.packageName) &&
                    snapshot.width == initialWidth &&
                    snapshot.height == initialHeight &&
                    completionMetrics.widthPixels == initialWidth &&
                    completionMetrics.heightPixels == initialHeight,
            ) { "Apps snapshot target or geometry changed during capture" }
            snapshot
        } finally {
            @Suppress("DEPRECATION")
            root.recycle()
        }
    }

    private fun requireActiveAppsTarget(target: String): Pair<Int, Int> {
        val root = service.rootInActiveWindow
            ?: throw IllegalArgumentException("Apps target has no active window")
        return try {
            val foreground = root.packageName?.toString()
            require(foreground == target) { "Apps foreground package does not match target" }
            require(protectedSystemSurface(root, target) == null) {
                "Apps owner refuses protected system surfaces"
            }
            require(!protectedApps.isProtected(target)) { "Apps owner refuses a protected app" }
            val metrics = service.resources.displayMetrics
            metrics.widthPixels to metrics.heightPixels
        } finally {
            @Suppress("DEPRECATION")
            root.recycle()
        }
    }

    private suspend fun waitForAppsForeground(target: String, shouldMatch: Boolean): Boolean {
        repeat(20) {
            if ((activeForegroundPackage() == target) == shouldMatch) return true
            delay(50L)
        }
        return (activeForegroundPackage() == target) == shouldMatch
    }

    private suspend fun verifyAppsMutationCompletion(
        claim: AppsOwnerClaim,
        mayLeaveTarget: Boolean,
    ): String? {
        val target = requireNotNull(claim.targetPackage)
        // Accessibility/global actions settle asynchronously even after their
        // completion callback. Sample the post-I/O package after one bounded
        // event-loop turn instead of accidentally reusing the pre-I/O root.
        delay(50L)
        if (!mayLeaveTarget) {
            requireActiveAppsTarget(target)
            val metrics = service.resources.displayMetrics
            val observation = requireNotNull(claim.observation)
            require(metrics.widthPixels == observation.width && metrics.heightPixels == observation.height) {
                "Apps geometry changed during mutation"
            }
            return target
        }
        return activeForegroundPackage()?.takeIf { it in claim.allowedPackages }
    }

    private fun appsOwnerTextResult(
        claim: AppsOwnerClaim,
        text: String,
        foregroundPackage: String?,
        snapshotSha256: String? = null,
        width: Int? = null,
        height: Int? = null,
        evidenceNodes: Int = 0,
        truncated: Boolean = false,
    ): McpToolCallResult {
        val bytes = text.toByteArray(Charsets.UTF_8)
        return appsOwnerResult(
            claim = claim,
            content = listOf(McpContentBlock(type = "text", text = text)),
            exactPayload = bytes,
            foregroundPackage = foregroundPackage,
            snapshotSha256 = snapshotSha256,
            width = width,
            height = height,
            evidenceNodes = evidenceNodes,
            truncated = truncated,
        )
    }

    private fun appsOwnerResult(
        claim: AppsOwnerClaim,
        content: List<McpContentBlock>,
        exactPayload: ByteArray,
        foregroundPackage: String?,
        snapshotSha256: String?,
        width: Int?,
        height: Int?,
        evidenceNodes: Int,
        truncated: Boolean,
    ): McpToolCallResult {
        val owner = appsOwnerResultReceipt(
            claim = claim,
            exactPayload = exactPayload,
            foregroundPackage = foregroundPackage,
            snapshotSha256 = snapshotSha256,
            width = width,
            height = height,
            evidenceNodes = evidenceNodes,
            truncated = truncated,
        )
        return McpToolCallResult(
            content = content,
            structuredContent = buildJsonObject { put("apps_owner", owner) },
        )
    }

    /**
     * Stamp the foreground package into `structuredContent` so the Magician
     * side can audit "what was done to which app" without a second round
     * trip. A result that already carries one — the protection refusal — is
     * left alone.
     *
     * The stamp is read at COMPLETION, not entry: the wait and scroll tools
     * can run for seconds and cross a foreground change, and an audit that
     * names the app that was on screen when the call *started* misattributes
     * what the tool actually observed. The `entryForeground` fallback covers
     * the moment the screen goes blank between completion and stamping.
     */
    /**
     * The rule `android_get_notifications` applies to a notification, applied
     * at the seam every screen-reading tool already passes through.
     *
     * A verification code reaches a run through the custody lane and must
     * never enter a tool result — the reasoning `handleGetNotifications`
     * states for itself. The shade is not an app, so the foreground gate
     * never fired for it, and the very message the notification tool withheld
     * came back verbatim from `android_get_ui_tree` as
     * `text="Your verification code is …"`. A heads-up banner puts the same
     * text over whatever app is open, so the test belongs on the content, not
     * on which surface happens to be showing.
     *
     * Redaction rather than refusal, for the reason the notification tools
     * already give: a code can arrive while the agent is legitimately reading
     * a screen, and refusing the whole read would only teach callers to poll
     * harder. `verification_text_withheld` tells the agent something was held
     * rather than absent — silence is what sends it looking for the same text
     * another way.
     *
     * The judgement itself lives in [VerificationTextScrub], where it can be
     * tested without a device.
     */
    private fun McpToolCallResult.withVerificationTextWithheld(
        toolName: String,
    ): McpToolCallResult {
        if (ProtectionGate.classify(toolName) != ProtectionGate.Disposition.FOREGROUND_GATED) {
            return this
        }
        var held = false
        val scrubbed = content.map { block ->
            val text = block.text
            if (block.type != "text" || text.isNullOrEmpty()) return@map block
            val result = VerificationTextScrub.scrub(text)
            if (!result.withheld) return@map block
            held = true
            block.copy(text = result.text)
        }
        if (!held) return this
        // The Apps owner response is decoded with deny-unknown-fields, so it
        // takes the scrubbed content without a sibling field it cannot parse.
        if (structuredContent?.containsKey("apps_owner") == true) {
            return copy(content = scrubbed)
        }
        val merged = buildJsonObject {
            structuredContent?.forEach { (key, value) -> put(key, value) }
            put("verification_text_withheld", true)
        }
        return copy(content = scrubbed, structuredContent = merged)
    }

    private fun McpToolCallResult.withForegroundPackage(entryForeground: String?): McpToolCallResult {
        // The Apps owner response has its own exact, signed-attribution shape.
        // Adding a legacy sibling field makes the runtime's deny-unknown-fields
        // decoder reject every otherwise valid snapshot.
        if (structuredContent?.containsKey("foreground_package") == true ||
            structuredContent?.containsKey("apps_owner") == true
        ) return this
        val completionForeground = activeForegroundPackage() ?: entryForeground ?: return this
        val merged = buildJsonObject {
            structuredContent?.forEach { (key, value) -> put(key, value) }
            put("foreground_package", completionForeground)
        }
        return copy(structuredContent = merged)
    }

    /**
     * Back and home stay available while a protected app is foregrounded:
     * navigating *out* of the app is the owner's benefit and observes
     * nothing. Everything else `android_press_key` can do — keycodes into
     * the focused field, recents (whose thumbnails show protected content),
     * the notification shade — stays gated.
     */
    private fun isNavigationEscape(toolName: String, args: JsonObject): Boolean =
        toolName == "android_press_key" &&
            args["key"]?.jsonPrimitive?.contentOrNull?.lowercase() in setOf("back", "home")

    /**
     * The point-of-use re-check. The entry gate saw one instant; a handler
     * that reads the screen later — or repeatedly — must ask again about the
     * screen it is actually reading. Returns the refusal to send, or null.
     */
    private fun activeProtectionRefusal(): McpToolCallResult? {
        val current = activeForegroundPackage()
        return if (protectedApps.isProtected(current)) ProtectionGate.refusal(current!!) else null
    }

    /** Extract a package from one owned root handle and always release it. */
    private fun activeForegroundPackage(): String? {
        val root = service.rootInActiveWindow ?: return null
        return try {
            root.packageName?.toString()
        } finally {
            @Suppress("DEPRECATION")
            root.recycle()
        }
    }

    private data class ActiveAppsSurface(
        val packageName: String?,
        val refusal: String?,
    )

    /** Sample completion package and window class from one owned root. */
    private fun activeAppsSurface(): ActiveAppsSurface {
        val root = service.rootInActiveWindow
            ?: return ActiveAppsSurface(null, "unknown_window")
        return try {
            val packageName = root.packageName?.toString()
            ActiveAppsSurface(
                packageName = packageName,
                refusal = if (packageName == null) {
                    "unknown_window"
                } else {
                    protectedSystemSurface(root, packageName)
                },
            )
        } finally {
            @Suppress("DEPRECATION")
            root.recycle()
        }
    }

    /**
     * System UI, the current launcher/recents owner, and non-application
     * windows can aggregate protected-app content under a different package.
     * Package allowlisting cannot safely attribute their descendants.
     */
    private fun protectedSystemSurface(
        root: android.view.accessibility.AccessibilityNodeInfo,
        foregroundPackage: String,
    ): String? {
        val homeIntent = Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_HOME)
        val launcherPackage = service.packageManager
            .resolveActivity(homeIntent, android.content.pm.PackageManager.MATCH_DEFAULT_ONLY)
            ?.activityInfo
            ?.packageName
        val window = root.window
        val windowKind = if (window == null) {
            AppsAccessibilityWindowKind.Unknown
        } else {
            try {
                if (window.type == android.view.accessibility.AccessibilityWindowInfo.TYPE_APPLICATION) {
                    AppsAccessibilityWindowKind.Application
                } else {
                    AppsAccessibilityWindowKind.NonApplication
                }
            } finally {
                @Suppress("DEPRECATION")
                window.recycle()
            }
        }
        return appsProtectedSystemSurfaceKind(
            foregroundPackage = foregroundPackage,
            launcherPackage = launcherPackage,
            windowKind = windowKind,
        )
    }

    // =====================================================================
    // GESTURE BRIDGE: callback → suspend
    // =====================================================================

    private suspend fun executeGestureAndWait(block: (GestureResultCallback) -> Unit): Boolean =
        suspendCancellableCoroutine { cont ->
            block(object : GestureResultCallback {
                override fun onCompleted(gesture: GestureDescription) {
                    if (cont.isActive) cont.resume(true)
                }
                override fun onCancelled(gesture: GestureDescription) {
                    if (cont.isActive) cont.resume(false)
                }
            })
            // AccessibilityService gestures cannot be cancelled once dispatched;
            // the callback guards with isActive so the result is silently discarded.
            cont.invokeOnCancellation { /* no-op: gesture runs to completion */ }
        }

    // =====================================================================
    // SELECTOR RESOLUTION
    // =====================================================================

    private fun resolveSelector(tree: UiTree, text: String?, resourceId: String?, contentDesc: String?): UiElement? {
        return tree.elements.firstOrNull { e ->
            (text != null && e.text?.contains(text, ignoreCase = true) == true) ||
            (resourceId != null && (e.resourceId?.endsWith(resourceId) == true || e.resourceId == resourceId)) ||
            (contentDesc != null && e.contentDescription?.contains(contentDesc, ignoreCase = true) == true)
        }
    }

    // =====================================================================
    // OBSERVE TOOLS
    // =====================================================================

    private suspend fun handleGetUiTree(args: JsonObject): McpToolCallResult {
        val includeInvisible = args["include_invisible"]?.jsonPrimitive?.booleanOrNull ?: false
        val maxDepth = args["max_depth"]?.jsonPrimitive?.intOrNull ?: 0
        val filter = args["filter"]?.jsonPrimitive?.contentOrNull ?: "interactive"

        val rootNode = service.rootInActiveWindow
            ?: return errorResult("No active window available")
        return try {
        val tree = uiTreeWalker.walkTree(rootNode, includeInvisible, maxDepth)

        val filtered = when (filter) {
            "all" -> tree.elements
            "text" -> tree.elements.filter { !it.text.isNullOrEmpty() || !it.contentDescription.isNullOrEmpty() }
            else -> tree.elements.filter { it.clickable || it.focusable || it.scrollable || it.checkable || !it.text.isNullOrEmpty() || !it.contentDescription.isNullOrEmpty() }
        }

        val table = buildString {
            append("IDX | resource_id | text | desc | flags | bounds\n")
            filtered.forEachIndexed { idx, e ->
                val flags = buildString {
                    if (e.clickable) append("c")
                    if (e.focusable) append("f")
                    if (e.scrollable) append("s")
                    if (e.checkable) append("k")
                }
                val bounds = e.bounds?.let { "[${it.left},${it.top},${it.right},${it.bottom}]" } ?: ""
                append("$idx | ${e.resourceId ?: ""} | ${e.text ?: ""} | ${e.contentDescription ?: ""} | $flags | $bounds\n")
            }
        }

        val result = buildJsonObject {
            put("format", "compact")
            put("app", tree.foregroundApp)
            put("total", tree.totalNodes)
            put("shown", filtered.size)
            put("filter", filter)
            put("elements", table.trimEnd())
        }
        textResult(result.toString())
        } finally {
            @Suppress("DEPRECATION")
            rootNode.recycle()
        }
    }

    private suspend fun handleScreenshot(args: JsonObject): McpToolCallResult {
        val quality = if (args["quality"]?.jsonPrimitive?.contentOrNull == "thumbnail")
            ScreenshotQuality.THUMBNAIL else ScreenshotQuality.FULL

        // Fresh check at the moment of capture: pixels are the one thing that
        // cannot be filtered after the fact.
        activeProtectionRefusal()?.let { return it }
        // Read the screen BEFORE capturing. Capture is not free of side
        // effects — it can raise a system dialog that takes focus — and a
        // device run proved a walk done afterwards saw a screen the shade had
        // already left, so the code went out unheld. The after-walk stays as
        // a union, for a code that appears while the shutter is open.
        val regionsBefore = verificationRegionsOnScreen()
        val jpegBytes = screenshotPipeline.capture(quality)
        val regionsAfter = verificationRegionsOnScreen()
        val held = holdVerificationRegions(jpegBytes, regionsBefore, regionsAfter)
            ?: return textResult(
                buildJsonObject {
                    put("error", "verification_region_not_holdable")
                    put(
                        "message",
                        "A verification code is on screen and this capture could not be " +
                            "held; the screenshot is refused rather than sent. Read the " +
                            "shade with android_notifications, or let the phone answer " +
                            "the ask itself.",
                    )
                }.toString(),
            )
        val base64 = Base64.encodeToString(held.bytes, Base64.NO_WRAP)
        val dm = service.resources.displayMetrics
        val meta = buildJsonObject {
            put("width", dm.widthPixels)
            put("height", dm.heightPixels)
            put("format", "jpeg")
            if (held.regions > 0) put("verification_regions_withheld", held.regions)
        }.toString()
        // Metadata first, image second. A base64 screenshot is tens of
        // thousands of characters, and every surface that shortens a payload
        // cuts from the end — so a `verification_regions_withheld` that
        // followed the image was present and unreadable, which is the same as
        // absent to the agent it is meant to warn.
        return McpToolCallResult(content = listOf(
            McpContentBlock(type = "text", text = meta),
            McpContentBlock(type = "image", data = base64, mimeType = "image/jpeg")
        ))
    }

    private class HeldScreenshot(val bytes: ByteArray, val regions: Int)

    /**
     * Where a verification code is showing, or `null` when the screen could
     * not be read at all.
     *
     * `null` is not "nothing there" — it is "no answer", and the caller
     * treats the two differently. Silently reporting an unreadable screen as
     * clean is how a guard stops guarding without anyone noticing.
     */
    private suspend fun verificationRegionsOnScreen(): List<Bounds>? {
        val root = service.rootInActiveWindow
        if (root == null) {
            Log.w(TAG, "verification hold: no active window to read; screen state unknown")
            return null
        }
        val elements = try {
            uiTreeWalker.walkTree(root, false, 0).elements
        } catch (error: Throwable) {
            Log.w(TAG, "verification hold: screen unreadable (${error.javaClass.simpleName})")
            return null
        }
        Log.i(TAG, "verification hold: read ${elements.size} node(s) from the screen")
        return elements
            .filter {
                VerificationTextScrub.carriesACode(it.text) ||
                    VerificationTextScrub.carriesACode(it.contentDescription)
            }
            .mapNotNull { it.bounds }
            .filter { it.right > it.left && it.bottom > it.top }
    }

    /**
     * The size of the screen the accessibility bounds are measured against.
     *
     * NOT `resources.displayMetrics`: that is the app-visible area, which
     * excludes the system bars, while a node's bounds and a screen capture
     * are both in true screen pixels. On a 720x1600 handset the app area is
     * 720x1520, and scaling by it pushed every blackout ~25px down — most of
     * a notification row, so the bar landed under the code instead of over
     * it. Verified by looking at the held image, which the audit count alone
     * could never have shown.
     */
    private fun realScreenSize(): Pair<Int, Int> {
        val fallback = service.resources.displayMetrics
        return try {
            val metrics = DisplayMetrics()
            @Suppress("DEPRECATION")
            (service.getSystemService(Context.WINDOW_SERVICE) as WindowManager)
                .defaultDisplay
                .getRealMetrics(metrics)
            if (metrics.widthPixels > 0 && metrics.heightPixels > 0) {
                metrics.widthPixels to metrics.heightPixels
            } else {
                fallback.widthPixels to fallback.heightPixels
            }
        } catch (_: Throwable) {
            fallback.widthPixels to fallback.heightPixels
        }
    }

    /** Is a code sitting in the shade, whether or not the screen shows it? */
    private fun anyActiveVerificationNotification(): Boolean =
        (NotificationListener.instance?.getActiveNotificationsList() ?: emptyList())
            .any { VerificationTextScrub.carriesACode(listOfNotNull(it.title, it.text).joinToString("\n")) }

    /**
     * Black out the parts of a capture that show a verification code.
     *
     * Text results are scrubbed at the shared seam, but a screenshot is
     * pixels, and a model that can see the image can read the code out of it
     * — the same leak wearing a different coat. The regions are judged by
     * [VerificationTextScrub.carriesACode], so a capture and a screen read
     * agree about what a code is.
     *
     * Held region by region rather than by refusing the tool: a verification
     * message can sit in the shade for days, and refusing every screenshot
     * while one does would take the phone's whole screen away for as long as
     * the message stays. The count travels back in the metadata so the agent
     * knows something was blacked out rather than merely absent.
     *
     * Returns `null` — the caller refuses — when a code could be on screen and
     * this could not prove otherwise: the screen was unreadable while a code
     * notification is live, or the image could not be held. Failing open in
     * either case would hand over the very pixels this exists to hide.
     */
    private fun holdVerificationRegions(
        jpegBytes: ByteArray,
        regionsBefore: List<Bounds>?,
        regionsAfter: List<Bounds>?,
    ): HeldScreenshot? {
        val regions = (regionsBefore.orEmpty() + regionsAfter.orEmpty()).distinct()
        if (regions.isEmpty()) {
            val screenUnreadable = regionsBefore == null && regionsAfter == null
            if (screenUnreadable && anyActiveVerificationNotification()) {
                Log.w(
                    TAG,
                    "verification hold: refusing a capture — a code notification is live " +
                        "and the screen could not be read",
                )
                return null
            }
            Log.i(
                TAG,
                "verification hold: nothing to hold (before=${regionsBefore?.size ?: -1}, " +
                    "after=${regionsAfter?.size ?: -1})",
            )
            return HeldScreenshot(jpegBytes, 0)
        }
        Log.i(TAG, "verification hold: blacking out ${regions.size} region(s) of a capture")
        return try {
            val decoded = BitmapFactory.decodeByteArray(jpegBytes, 0, jpegBytes.size) ?: return null
            val canvasBitmap = decoded.copy(Bitmap.Config.ARGB_8888, true) ?: return null
            val (screenWidth, screenHeight) = realScreenSize()
            val scaleX = canvasBitmap.width.toFloat() / screenWidth.toFloat()
            val scaleY = canvasBitmap.height.toFloat() / screenHeight.toFloat()
            val canvas = Canvas(canvasBitmap)
            val paint = Paint().apply {
                color = Color.BLACK
                style = Paint.Style.FILL
            }
            regions.forEach { bounds ->
                // Cover a little more than the node, never a little less:
                // rounding and text antialiasing at the edge of a box are the
                // difference between a hidden digit and a legible one.
                canvas.drawRect(
                    bounds.left * scaleX - REDACTION_MARGIN_PX,
                    bounds.top * scaleY - REDACTION_MARGIN_PX,
                    bounds.right * scaleX + REDACTION_MARGIN_PX,
                    bounds.bottom * scaleY + REDACTION_MARGIN_PX,
                    paint,
                )
            }
            val out = ByteArrayOutputStream()
            val compressed = canvasBitmap.compress(Bitmap.CompressFormat.JPEG, 90, out)
            canvasBitmap.recycle()
            if (!compressed) null else HeldScreenshot(out.toByteArray(), regions.size)
        } catch (_: Throwable) {
            null
        }
    }

    private suspend fun handleFindElements(args: JsonObject): McpToolCallResult {
        val text = args["text"]?.jsonPrimitive?.contentOrNull
        val resourceId = args["resource_id"]?.jsonPrimitive?.contentOrNull
        val contentDesc = args["content_desc"]?.jsonPrimitive?.contentOrNull
        val className = args["class_name"]?.jsonPrimitive?.contentOrNull
        val findAll = args["find_all"]?.jsonPrimitive?.booleanOrNull ?: false

        if (text == null && resourceId == null && contentDesc == null && className == null) {
            return errorResult("At least one selector (text, resource_id, content_desc, class_name) required")
        }

        val rootNode = service.rootInActiveWindow ?: return errorResult("No active window")
        try {
        // Point-of-use re-check: gate the root this handler actually walks,
        // not the one the entry gate glimpsed. A null root at entry passes
        // the gate, and a protected window can attach in between.
        rootNode.packageName?.toString()?.let { pkg ->
            if (protectedApps.isProtected(pkg)) return ProtectionGate.refusal(pkg)
        }
        val tree = uiTreeWalker.walkTree(rootNode)

        val matches = tree.elements.filter { e ->
            (text != null && e.text?.contains(text, ignoreCase = true) == true) ||
            (resourceId != null && (e.resourceId?.endsWith(resourceId) == true)) ||
            (contentDesc != null && e.contentDescription?.contains(contentDesc, ignoreCase = true) == true) ||
            (className != null && e.className?.contains(className) == true)
        }.let { if (findAll) it else if (it.isNotEmpty()) listOf(it.first()) else it }

        val result = buildJsonObject {
            putJsonArray("elements") {
                matches.forEach { e ->
                    addJsonObject {
                        put("elementId", e.elementId)
                        e.resourceId?.let { put("resourceId", it) }
                        e.text?.let { put("text", it) }
                        e.contentDescription?.let { put("contentDescription", it) }
                        e.bounds?.let { b -> put("bounds", "[${b.left},${b.top},${b.right},${b.bottom}]") }
                        put("clickable", e.clickable)
                    }
                }
            }
            put("total_matches", matches.size)
        }
        return textResult(result.toString())
        } finally {
            @Suppress("DEPRECATION")
            rootNode.recycle()
        }
    }

    private suspend fun handleGetScreenContext(args: JsonObject): McpToolCallResult {
        val includeAll = args["include_all_elements"]?.jsonPrimitive?.booleanOrNull ?: false
        val rootNode = service.rootInActiveWindow ?: return errorResult("No active window")
        try {
        // Point-of-use re-check: gate the root this handler actually walks,
        // not the one the entry gate glimpsed. A null root at entry passes
        // the gate, and a protected window can attach in between.
        rootNode.packageName?.toString()?.let { pkg ->
            if (protectedApps.isProtected(pkg)) return ProtectionGate.refusal(pkg)
        }
        val tree = uiTreeWalker.walkTree(rootNode)
        val jpegBytes = screenshotPipeline.capture(ScreenshotQuality.THUMBNAIL)
        val base64 = Base64.encodeToString(jpegBytes, Base64.NO_WRAP)

        val filtered = if (includeAll) tree.elements
        else tree.elements.filter { it.clickable || it.focusable || it.checkable || it.scrollable || !it.text.isNullOrEmpty() }

        val meta = buildJsonObject {
            putJsonObject("app_info") { put("package_name", tree.foregroundApp) }
            putJsonObject("ui_tree") {
                put("total_elements", tree.totalNodes)
                put("filtered_elements", filtered.size)
                putJsonArray("elements") {
                    filtered.forEach { e ->
                        addJsonObject {
                            e.resourceId?.let { put("resourceId", it) }
                            e.text?.let { put("text", it) }
                            e.bounds?.let { b ->
                                put("center_x", (b.left + b.right) / 2)
                                put("center_y", (b.top + b.bottom) / 2)
                                put("bounds", "[${b.left},${b.top},${b.right},${b.bottom}]")
                            }
                        }
                    }
                }
            }
        }.toString()

        return McpToolCallResult(content = listOf(
            McpContentBlock(type = "text", text = meta),
            McpContentBlock(type = "image", data = base64, mimeType = "image/jpeg")
        ))
        } finally {
            @Suppress("DEPRECATION")
            rootNode.recycle()
        }
    }

    /**
     * Wait for a one-time code and return only that.
     *
     * Deliberately not a filter over `android_get_notifications`. That tool
     * hands back every notification body on the phone, and an agent that reads
     * them all to find six digits keeps the rest in context. This returns the
     * code and who sent it, and never the surrounding message.
     */
    /**
     * Watch this phone's notifications for one verification challenge and
     * answer the challenge itself (secure HITL plan §6.2, P6).
     *
     * The runtime names the challenge — correlation id, source, the window
     * anchored on the server's own start, the deadline, the expected length.
     * A matching code is posted to `/hitl/{id}/respond` over this device's
     * paired credential; what goes back over MCP is status. Digits never leave
     * the phone through a tool result: a call without a challenge is refused
     * rather than served the old way, and a stale code from before the window
     * is never considered.
     */
    private suspend fun handleAwaitOtp(args: JsonObject): McpToolCallResult {
        val timeoutSeconds = (args["timeout_seconds"]?.jsonPrimitive?.intOrNull ?: 60).coerceIn(1, 300)
        val challenge = args["challenge"]?.jsonObject
            ?: return textResult(statusJson("requires_challenge", "android_await_otp answers a named verification challenge; it never returns a code"))
        val correlationId = challenge["correlation_id"]?.jsonPrimitive?.contentOrNull?.trim().orEmpty()
        if (correlationId.isEmpty()) {
            return textResult(statusJson("requires_challenge", "the challenge names no correlation id"))
        }
        // The runtime names the lane that owns the ask. An older runtime that
        // sends none means an agentic pause: `user_request` has no record of
        // one, so defaulting to it refused every answer this phone deposited.
        val source = challenge["source"]?.jsonPrimitive?.contentOrNull?.takeIf { it.isNotBlank() } ?: "agentic"
        val now = System.currentTimeMillis()
        val windowStartMs = challenge["window_start_ms"]?.jsonPrimitive?.longOrNull
            ?: challenge["started_at_ms"]?.jsonPrimitive?.longOrNull
            ?: now
        val deadlineMs = challenge["deadline_ms"]?.jsonPrimitive?.longOrNull
        val expectedDigits = challenge["expected_digits"]?.jsonPrimitive?.intOrNull

        val listener = NotificationListener.instance
            ?: return textResult(
                statusJson(
                    "unavailable",
                    "Notification access is not granted. Settings > Notifications > Notification access > Magdroid.",
                ),
            )

        val deadline = minOf(now + timeoutSeconds * 1000L, deadlineMs ?: Long.MAX_VALUE)
        val deposit = ChallengeDeposit(service)
        try {
            while (System.currentTimeMillis() < deadline) {
                // A protected app's OTP can never match, whatever is
                // foregrounded: the code IS the thing the protection exists to
                // keep on-device.
                val notifications = listener.getActiveNotificationsList()
                    .filterNot { protectedApps.isProtected(it.packageName) }
                when (val decision = OtpWatcher.decide(notifications, windowStartMs, deadlineMs, expectedDigits)) {
                    is OtpWatcher.Decision.Code -> {
                        val outcome = deposit.answer(correlationId, source, decision.found.code)
                        val status = when (outcome) {
                            ChallengeDeposit.Outcome.Deposited -> "deposited"
                            ChallengeDeposit.Outcome.AlreadyResolved -> "already_resolved"
                            is ChallengeDeposit.Outcome.Refused -> "deposit_failed"
                        }
                        return textResult(
                            buildJsonObject {
                                put("status", status)
                                put("sender", decision.found.sender)
                                put("waited_ms", System.currentTimeMillis() - now)
                                if (outcome is ChallengeDeposit.Outcome.Refused) put("reason", outcome.reason)
                            }.toString(),
                        )
                    }
                    is OtpWatcher.Decision.Ambiguous -> return textResult(
                        buildJsonObject {
                            put("status", "ambiguous")
                            put("candidates", decision.candidates)
                            put("waited_ms", System.currentTimeMillis() - now)
                        }.toString(),
                    )
                    OtpWatcher.Decision.None -> {}
                }
                kotlinx.coroutines.delay(POLL_INTERVAL_MS)
            }
        } finally {
            deposit.close()
        }
        return textResult(
            statusJson(
                "no_code",
                "No verification message arrived inside the challenge's window.",
            ),
        )
    }

    private fun statusJson(status: String, message: String): String = buildJsonObject {
        put("status", status)
        put("message", message)
    }.toString()

    private fun handleGetNotifications(args: JsonObject): McpToolCallResult {
        val notifications = (NotificationListener.instance?.getActiveNotificationsList() ?: emptyList())
            .filterNot { protectedApps.isProtected(it.packageName) }
        var withheld = 0
        val result = buildJsonObject {
            putJsonArray("notifications") {
                notifications.forEach { n ->
                    addJsonObject {
                        put("package_name", n.packageName)
                        // A verification message is never read out here. The
                        // whole point of `android_await_otp` — the code answers
                        // the ask itself and never enters a tool result — is
                        // undone if the adjacent read tool hands the model the
                        // same digits, and the apps codes actually arrive in
                        // (Messages, Gmail) are not protected apps.
                        val carriesACode = OtpWatcher.extractCode(
                            listOfNotNull(n.title, n.text).joinToString("\n"),
                        ) !is OtpWatcher.Extraction.None
                        if (carriesACode) {
                            withheld += 1
                            put("title", CODE_WITHHELD)
                            put("text", CODE_WITHHELD)
                            put("withheld", true)
                        } else {
                            put("title", n.title)
                            put("text", n.text)
                        }
                        put("post_time", n.postTime)
                        put("ongoing", n.ongoing)
                        put("clearable", n.clearable)
                    }
                }
            }
            put("count", notifications.size)
            put("withheld_count", withheld)
        }
        return textResult(result.toString())
    }

    private suspend fun handleAccessibilityAudit(args: JsonObject): McpToolCallResult {
        val rootNode = service.rootInActiveWindow ?: return errorResult("No active window")
        try {
        // Point-of-use re-check: gate the root this handler actually walks,
        // not the one the entry gate glimpsed. A null root at entry passes
        // the gate, and a protected window can attach in between.
        rootNode.packageName?.toString()?.let { pkg ->
            if (protectedApps.isProtected(pkg)) return ProtectionGate.refusal(pkg)
        }
        val tree = uiTreeWalker.walkTree(rootNode, includeInvisible = false)

        val issues = mutableListOf<String>()
        tree.elements.forEach { e ->
            if (e.clickable && e.contentDescription.isNullOrEmpty() && e.text.isNullOrEmpty()) {
                val bounds = e.bounds?.let { "[${it.left},${it.top},${it.right},${it.bottom}]" } ?: "unknown"
                issues.add("Missing content description on clickable element (${e.className ?: "unknown"}) at $bounds")
            }
            e.bounds?.let { b ->
                val width = b.right - b.left
                val height = b.bottom - b.top
                val dm = service.resources.displayMetrics
                val minPx = (48 * dm.density).toInt()
                if (e.clickable && (width < minPx || height < minPx)) {
                    issues.add("Small touch target ${width}x${height}px (min ${minPx}px) on ${e.text ?: e.contentDescription ?: e.className ?: "element"}")
                }
            }
        }

        val result = buildJsonObject {
            put("issues_found", issues.size)
            put("pass", issues.isEmpty())
            putJsonArray("issues") { issues.forEach { add(JsonPrimitive(it)) } }
        }
        return textResult(result.toString())
        } finally {
            @Suppress("DEPRECATION")
            rootNode.recycle()
        }
    }

    private suspend fun handleScreenshotDiff(args: JsonObject): McpToolCallResult {
        val referenceBase64 = args["reference_base64"]?.jsonPrimitive?.contentOrNull
            ?: return errorResult("reference_base64 is required")
        val threshold = args["threshold"]?.jsonPrimitive?.doubleOrNull ?: 0.95

        // Same rule as handleScreenshot: check at the moment of capture.
        activeProtectionRefusal()?.let { return it }
        val currentBytes = screenshotPipeline.capture(ScreenshotQuality.THUMBNAIL)
        val referenceBytes = Base64.decode(referenceBase64, Base64.DEFAULT)

        val currentBitmap = android.graphics.BitmapFactory.decodeByteArray(currentBytes, 0, currentBytes.size)
            ?: return errorResult("Failed to decode current screenshot as bitmap")
        val referenceBitmap = android.graphics.BitmapFactory.decodeByteArray(referenceBytes, 0, referenceBytes.size)
            ?: return errorResult("Failed to decode reference_base64 as bitmap")

        // Scale reference to current dimensions if needed
        val scaledRef = if (referenceBitmap.width != currentBitmap.width ||
                            referenceBitmap.height != currentBitmap.height) {
            android.graphics.Bitmap.createScaledBitmap(
                referenceBitmap, currentBitmap.width, currentBitmap.height, true)
        } else referenceBitmap

        val width = currentBitmap.width
        val height = currentBitmap.height
        val currentPixels = IntArray(width * height)
        val refPixels = IntArray(width * height)
        currentBitmap.getPixels(currentPixels, 0, width, 0, 0, width, height)
        scaledRef.getPixels(refPixels, 0, width, 0, 0, width, height)

        // Count pixels within ±10 per channel (RGB)
        var matching = 0L
        for (i in currentPixels.indices) {
            val c1 = currentPixels[i]; val c2 = refPixels[i]
            val rDiff = ((c1 shr 16 and 0xFF) - (c2 shr 16 and 0xFF)).let { if (it < 0) -it else it }
            val gDiff = ((c1 shr 8 and 0xFF) - (c2 shr 8 and 0xFF)).let { if (it < 0) -it else it }
            val bDiff = ((c1 and 0xFF) - (c2 and 0xFF)).let { if (it < 0) -it else it }
            if (rDiff + gDiff + bDiff <= 30) matching++
        }
        val similarity = if (currentPixels.isNotEmpty()) matching.toDouble() / currentPixels.size else 0.0
        val matches = similarity >= threshold

        val result = buildJsonObject {
            put("similarity", similarity)
            put("threshold", threshold)
            put("matches", matches)
        }
        return textResult(result.toString())
    }

    private fun handleGetRecentToasts(args: JsonObject): McpToolCallResult {
        val sinceMs = args["since_ms"]?.jsonPrimitive?.longOrNull ?: 5000L
        val cutoff = System.currentTimeMillis() - sinceMs
        // A toast whose source cannot be attributed is kept — dropping it
        // would make attribution failures silently censor benign toasts —
        // but anything attributed to a protected app stays on the device.
        val toasts = MagdroidAccessibilityService.recentToasts
            .filter { it.timestamp >= cutoff }
            .filterNot { protectedApps.isProtected(it.packageName) }
        val result = buildJsonObject {
            putJsonArray("toasts") {
                toasts.forEach { toast ->
                    addJsonObject {
                        put("text", toast.text)
                        put("timestamp", toast.timestamp)
                    }
                }
            }
        }
        return textResult(result.toString())
    }

    // =====================================================================
    // ACT TOOLS
    // =====================================================================

    private suspend fun handleTap(args: JsonObject): McpToolCallResult {
        val x = args["x"]?.jsonPrimitive?.intOrNull
        val y = args["y"]?.jsonPrimitive?.intOrNull
        val text = args["text"]?.jsonPrimitive?.contentOrNull
        val resourceId = args["resource_id"]?.jsonPrimitive?.contentOrNull
        val contentDesc = args["content_desc"]?.jsonPrimitive?.contentOrNull

        val (tapX, tapY) = if (x != null && y != null) {
            x.toFloat() to y.toFloat()
        } else {
            val rootNode = service.rootInActiveWindow ?: return errorResult("No active window")
            try {
        // Point-of-use re-check: gate the root this handler actually walks,
        // not the one the entry gate glimpsed. A null root at entry passes
        // the gate, and a protected window can attach in between.
        rootNode.packageName?.toString()?.let { pkg ->
            if (protectedApps.isProtected(pkg)) return ProtectionGate.refusal(pkg)
        }
            val tree = uiTreeWalker.walkTree(rootNode)
            val element = resolveSelector(tree, text, resourceId, contentDesc)
                ?: return errorResult("Element not found: text=$text, resource_id=$resourceId, content_desc=$contentDesc")
            val b = element.bounds ?: return errorResult("Element has no bounds")
            ((b.left + b.right) / 2).toFloat() to ((b.top + b.bottom) / 2).toFloat()
            } finally {
                @Suppress("DEPRECATION")
                rootNode.recycle()
            }
        }

        val success = withTimeoutOrNull(5000L) {
            executeGestureAndWait { cb -> gestureEngine.executeTap(tapX, tapY, cb) }
        } ?: false

        return if (success) textResult("{\"latency_ms\":0}")
        else errorResult("Tap gesture was cancelled or timed out")
    }

    private suspend fun handleLongPress(args: JsonObject): McpToolCallResult {
        val x = args["x"]?.jsonPrimitive?.intOrNull?.toFloat()
        val y = args["y"]?.jsonPrimitive?.intOrNull?.toFloat()
        val durationMs = (args["duration_ms"]?.jsonPrimitive?.longOrNull ?: 1000L).coerceIn(1L, MAX_GESTURE_DURATION_MS)

        val (lx, ly) = if (x != null && y != null) {
            x to y
        } else {
            val text = args["text"]?.jsonPrimitive?.contentOrNull
            val resourceId = args["resource_id"]?.jsonPrimitive?.contentOrNull
            val rootNode = service.rootInActiveWindow ?: return errorResult("No active window")
            try {
        // Point-of-use re-check: gate the root this handler actually walks,
        // not the one the entry gate glimpsed. A null root at entry passes
        // the gate, and a protected window can attach in between.
        rootNode.packageName?.toString()?.let { pkg ->
            if (protectedApps.isProtected(pkg)) return ProtectionGate.refusal(pkg)
        }
            val tree = uiTreeWalker.walkTree(rootNode)
            val element = resolveSelector(tree, text, resourceId, null)
                ?: return errorResult("Element not found")
            val b = element.bounds ?: return errorResult("Element has no bounds")
            ((b.left + b.right) / 2).toFloat() to ((b.top + b.bottom) / 2).toFloat()
            } finally {
                @Suppress("DEPRECATION")
                rootNode.recycle()
            }
        }

        val success = withTimeoutOrNull(durationMs + 2000L) {
            executeGestureAndWait { cb -> gestureEngine.executeLongPress(lx, ly, durationMs, cb) }
        } ?: false
        return if (success) textResult("{\"latency_ms\":0}") else errorResult("Long press cancelled")
    }

    private suspend fun handleDoubleTap(args: JsonObject): McpToolCallResult {
        val x = args["x"]?.jsonPrimitive?.intOrNull?.toFloat()
        val y = args["y"]?.jsonPrimitive?.intOrNull?.toFloat()

        val (dx, dy) = if (x != null && y != null) {
            x to y
        } else {
            val text = args["text"]?.jsonPrimitive?.contentOrNull
            val resourceId = args["resource_id"]?.jsonPrimitive?.contentOrNull
            val contentDesc = args["content_desc"]?.jsonPrimitive?.contentOrNull
            val rootNode = service.rootInActiveWindow ?: return errorResult("No active window")
            try {
        // Point-of-use re-check: gate the root this handler actually walks,
        // not the one the entry gate glimpsed. A null root at entry passes
        // the gate, and a protected window can attach in between.
        rootNode.packageName?.toString()?.let { pkg ->
            if (protectedApps.isProtected(pkg)) return ProtectionGate.refusal(pkg)
        }
            val tree = uiTreeWalker.walkTree(rootNode)
            val element = resolveSelector(tree, text, resourceId, contentDesc)
                ?: return errorResult("Element not found")
            val b = element.bounds ?: return errorResult("Element has no bounds")
            ((b.left + b.right) / 2).toFloat() to ((b.top + b.bottom) / 2).toFloat()
            } finally {
                @Suppress("DEPRECATION")
                rootNode.recycle()
            }
        }

        val success = withTimeoutOrNull(5000L) {
            executeGestureAndWait { cb -> gestureEngine.executeDoubleTap(dx, dy, cb) }
        } ?: false
        return if (success) textResult("{\"latency_ms\":0}") else errorResult("Double tap cancelled")
    }

    private suspend fun handleSwipe(args: JsonObject): McpToolCallResult {
        val startX = args["start_x"]?.jsonPrimitive?.intOrNull?.toFloat() ?: return errorResult("start_x required")
        val startY = args["start_y"]?.jsonPrimitive?.intOrNull?.toFloat() ?: return errorResult("start_y required")
        val endX = args["end_x"]?.jsonPrimitive?.intOrNull?.toFloat() ?: return errorResult("end_x required")
        val endY = args["end_y"]?.jsonPrimitive?.intOrNull?.toFloat() ?: return errorResult("end_y required")
        val durationMs = (args["duration_ms"]?.jsonPrimitive?.longOrNull ?: 300L).coerceIn(1L, MAX_GESTURE_DURATION_MS)

        val success = withTimeoutOrNull(durationMs + 2000L) {
            executeGestureAndWait { cb -> gestureEngine.executeSwipe(startX, startY, endX, endY, durationMs, cb) }
        } ?: false
        return if (success) textResult("{\"latency_ms\":0}") else errorResult("Swipe cancelled")
    }

    private suspend fun handlePinch(args: JsonObject): McpToolCallResult {
        val centerX = args["center_x"]?.jsonPrimitive?.intOrNull?.toFloat() ?: return errorResult("center_x required")
        val centerY = args["center_y"]?.jsonPrimitive?.intOrNull?.toFloat() ?: return errorResult("center_y required")
        val scale = args["scale"]?.jsonPrimitive?.floatOrNull ?: return errorResult("scale required")
        val durationMs = (args["duration_ms"]?.jsonPrimitive?.longOrNull ?: 300L).coerceIn(1L, MAX_GESTURE_DURATION_MS)

        val success = withTimeoutOrNull(durationMs + 2000L) {
            executeGestureAndWait { cb -> gestureEngine.executePinch(centerX, centerY, scale, durationMs, cb) }
        } ?: false
        return if (success) textResult("{\"latency_ms\":0}") else errorResult("Pinch cancelled")
    }

    private suspend fun handleDrag(args: JsonObject): McpToolCallResult {
        val fromX = args["from_x"]?.jsonPrimitive?.intOrNull?.toFloat() ?: return errorResult("from_x required")
        val fromY = args["from_y"]?.jsonPrimitive?.intOrNull?.toFloat() ?: return errorResult("from_y required")
        val toX = args["to_x"]?.jsonPrimitive?.intOrNull?.toFloat() ?: return errorResult("to_x required")
        val toY = args["to_y"]?.jsonPrimitive?.intOrNull?.toFloat() ?: return errorResult("to_y required")
        val durationMs = (args["duration_ms"]?.jsonPrimitive?.longOrNull ?: 1000L).coerceIn(1L, MAX_GESTURE_DURATION_MS)

        val success = withTimeoutOrNull(durationMs + 2000L) {
            executeGestureAndWait { cb -> gestureEngine.executeDrag(fromX, fromY, toX, toY, durationMs, cb) }
        } ?: false
        return if (success) textResult("{\"latency_ms\":0}") else errorResult("Drag cancelled")
    }

    private suspend fun handleInputText(args: JsonObject): McpToolCallResult {
        val text = args["text"]?.jsonPrimitive?.contentOrNull ?: return errorResult("text required")
        val append = args["append"]?.jsonPrimitive?.booleanOrNull ?: false
        val resourceId = args["resource_id"]?.jsonPrimitive?.contentOrNull
        val elementText = args["element_text"]?.jsonPrimitive?.contentOrNull

        // With no selector, type into whatever currently holds input focus.
        //
        // An empty, unlabelled field — a phone number box, an OTP box, most
        // login forms — has neither a resource id nor any text, so
        // identifier-based lookup cannot reach it even after the field has been
        // tapped. `findFocus` needs no identifier, which is exactly the case
        // identifiers cannot serve.
        if (resourceId == null && elementText == null) {
            val focused = service.findFocus(android.view.accessibility.AccessibilityNodeInfo.FOCUS_INPUT)
            if (focused != null) {
                val typed = try {
                    inputEngine.inputText(focused, text, append)
                } finally {
                    @Suppress("DEPRECATION")
                    focused.recycle()
                }
                if (typed) return textResult("{\"latency_ms\":0,\"target\":\"focused\"}")
                // Fall through to the tree scan rather than failing: a focused
                // node that refuses SET_TEXT is not the same as no field at all.
            }
        }

        val rootNode = service.rootInActiveWindow ?: return errorResult("No active window")
        try {
        // Point-of-use re-check: gate the root this handler actually walks,
        // not the one the entry gate glimpsed. A null root at entry passes
        // the gate, and a protected window can attach in between.
        rootNode.packageName?.toString()?.let { pkg ->
            if (protectedApps.isProtected(pkg)) return ProtectionGate.refusal(pkg)
        }
        val tree = uiTreeWalker.walkTree(rootNode)

        val target = if (resourceId != null || elementText != null) {
            resolveSelector(tree, elementText, resourceId, null)
                ?: return errorResult("Element not found: resource_id=$resourceId, text=$elementText")
        } else {
            tree.elements.firstOrNull { e ->
                e.focusable && (
                    e.className?.contains("EditText") == true ||
                    e.className?.contains("TextField") == true ||
                    e.className == "android.widget.AutoCompleteTextView" ||
                    e.className?.contains("SearchView") == true
                )
            }
                ?: return errorResult("No editable element found. Specify resource_id or element_text")
        }

        // Walk node tree to find matching AccessibilityNodeInfo by resource ID or text
        val nodeInfo = findNodeByElement(rootNode, target.resourceId, target.text)
            ?: service.findFocus(android.view.accessibility.AccessibilityNodeInfo.FOCUS_INPUT)
            ?: return errorResult(
                "Could not get node reference for element, and nothing holds input focus. " +
                    "Tap the field first, or pass resource_id/element_text."
            )

        val success = try {
            inputEngine.inputText(nodeInfo, text, append)
        } finally {
            @Suppress("DEPRECATION")
            nodeInfo.recycle()
        }
        return if (success) textResult("{\"latency_ms\":0}") else errorResult("Input text failed")
        } finally {
            @Suppress("DEPRECATION")
            rootNode.recycle()
        }
    }

    private fun findNodeByElement(
        root: android.view.accessibility.AccessibilityNodeInfo,
        resourceId: String?,
        text: String?
    ): android.view.accessibility.AccessibilityNodeInfo? {
        if (resourceId != null) {
            val results = root.findAccessibilityNodeInfosByViewId(resourceId)
            if (results.isNotEmpty()) {
                results.drop(1).forEach { node ->
                    @Suppress("DEPRECATION")
                    node.recycle()
                }
                return results.first()
            }
        }
        if (text != null) {
            val results = root.findAccessibilityNodeInfosByText(text)
            if (results.isNotEmpty()) {
                results.drop(1).forEach { node ->
                    @Suppress("DEPRECATION")
                    node.recycle()
                }
                return results.first()
            }
        }
        return null
    }

    private fun handlePressKey(args: JsonObject): McpToolCallResult {
        val key = args["key"]?.jsonPrimitive?.contentOrNull ?: return errorResult("key required")

        // The same surfaces handleGlobalAction refuses, reachable by key name.
        if (
            key.lowercase() in setOf("recents", "notifications") &&
            protectedApps.packages.value.isNotEmpty()
        ) {
            return ProtectionGate.surfaceRefusal(key.lowercase())
        }

        // Global actions first (don't need a focused node)
        val globalAction = when (key.lowercase()) {
            "back" -> android.accessibilityservice.AccessibilityService.GLOBAL_ACTION_BACK
            "home" -> android.accessibilityservice.AccessibilityService.GLOBAL_ACTION_HOME
            "recents" -> android.accessibilityservice.AccessibilityService.GLOBAL_ACTION_RECENTS
            "notifications" -> android.accessibilityservice.AccessibilityService.GLOBAL_ACTION_NOTIFICATIONS
            "power" -> android.accessibilityservice.AccessibilityService.GLOBAL_ACTION_LOCK_SCREEN
            else -> null
        }
        if (globalAction != null) {
            service.performGlobalAction(globalAction)
            return textResult("{\"key\":\"$key\",\"latency_ms\":0}")
        }

        // For other keys, delegate to InputEngine
        val rootNode = service.rootInActiveWindow
        val focusedNode = try {
            rootNode?.findFocus(android.view.accessibility.AccessibilityNodeInfo.FOCUS_INPUT)
        } finally {
            @Suppress("DEPRECATION")
            rootNode?.recycle()
        }
        val success = try {
            inputEngine.pressKey(key, focusedNode)
        } finally {
            @Suppress("DEPRECATION")
            focusedNode?.recycle()
        }
        return if (success) textResult("{\"key\":\"$key\",\"latency_ms\":0}")
        else errorResult("Key '$key' not supported or no focused input field. Supported: back, home, recents, notifications, power (global); enter, delete/backspace, tab, escape, space, select_all, cut, copy, paste (requires focused field).")
    }

    private fun handleGlobalAction(args: JsonObject): McpToolCallResult {
        val action = args["action"]?.jsonPrimitive?.contentOrNull ?: return errorResult("action required")
        // Recents shows every protected app's live thumbnail under the
        // LAUNCHER's package name, and the notification shade shows the
        // notification text the notification tools filter — both route a
        // screenshot cleanly around a foreground-package gate. While anything
        // is protected, the agent does not get to open either surface. Back
        // and home stay free: navigation observes nothing.
        if (
            action.lowercase() in setOf("recents", "notifications") &&
            protectedApps.packages.value.isNotEmpty()
        ) {
            return ProtectionGate.surfaceRefusal(action.lowercase())
        }
        val globalAction = when (action.lowercase()) {
            "back" -> android.accessibilityservice.AccessibilityService.GLOBAL_ACTION_BACK
            "home" -> android.accessibilityservice.AccessibilityService.GLOBAL_ACTION_HOME
            "recents" -> android.accessibilityservice.AccessibilityService.GLOBAL_ACTION_RECENTS
            "notifications" -> android.accessibilityservice.AccessibilityService.GLOBAL_ACTION_NOTIFICATIONS
            "quick_settings" -> android.accessibilityservice.AccessibilityService.GLOBAL_ACTION_QUICK_SETTINGS
            else -> return errorResult("Unknown action: $action. Valid: back, home, recents, notifications, quick_settings")
        }
        val success = service.performGlobalAction(globalAction)
        return if (success) textResult("{\"action\":\"$action\",\"latency_ms\":0}")
        else errorResult("Global action '$action' failed")
    }

    // =====================================================================
    // MANAGE TOOLS
    // =====================================================================

    private fun handleLaunchApp(args: JsonObject): McpToolCallResult {
        val packageName = args["package_name"]?.jsonPrimitive?.contentOrNull ?: return errorResult("package_name required")
        val clearTask = args["clear_task"]?.jsonPrimitive?.booleanOrNull ?: false

        val pm = service.packageManager
        val launchIntent = pm.getLaunchIntentForPackage(packageName)
            ?: return errorResult("No launch intent found for package: $packageName")
        if (clearTask) launchIntent.addFlags(Intent.FLAG_ACTIVITY_CLEAR_TASK or Intent.FLAG_ACTIVITY_NEW_TASK)
        else launchIntent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        service.startActivity(launchIntent)
        return textResult("{\"package_name\":\"$packageName\",\"latency_ms\":0}")
    }

    private fun handleCloseApp(args: JsonObject): McpToolCallResult {
        service.performGlobalAction(android.accessibilityservice.AccessibilityService.GLOBAL_ACTION_HOME)
        return textResult("{\"success\":true,\"note\":\"Moved to background via HOME.\"}")
    }

    private fun handleOpenUrl(args: JsonObject): McpToolCallResult {
        val url = args["url"]?.jsonPrimitive?.contentOrNull ?: return errorResult("url required")
        val intent = Intent(Intent.ACTION_VIEW, Uri.parse(url)).apply {
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        }
        // A URL is not a launch. `android_launch_app` opens a protected app at
        // its front door with no parameters, which is the owner's benefit; a
        // deep link carries agent-chosen state into an agent-chosen screen —
        // `bank://pay?to=…` is the canonical abuse. Resolve where the view
        // intent actually lands, and refuse a protected destination.
        val target = service.packageManager
            .resolveActivity(intent, android.content.pm.PackageManager.MATCH_DEFAULT_ONLY)
            ?.activityInfo?.packageName
        if (protectedApps.isProtected(target)) {
            return ProtectionGate.refusal(target!!)
        }
        service.startActivity(intent)
        return textResult("{\"url\":\"$url\",\"latency_ms\":0}")
    }

    private fun handleSetClipboard(args: JsonObject): McpToolCallResult {
        val text = args["text"]?.jsonPrimitive?.contentOrNull ?: return errorResult("text required")
        inputEngine.setClipboardText(text)
        return textResult("{\"success\":true}")
    }

    private fun handleListApps(args: JsonObject): McpToolCallResult {
        val filter = args["filter"]?.jsonPrimitive?.contentOrNull ?: "all"
        val pm = service.packageManager
        val packages = pm.getInstalledPackages(0)
        val filtered = when (filter) {
            "system" -> packages.filter { (it.applicationInfo.flags and android.content.pm.ApplicationInfo.FLAG_SYSTEM) != 0 }
            "third_party" -> packages.filter { (it.applicationInfo.flags and android.content.pm.ApplicationInfo.FLAG_SYSTEM) == 0 }
            else -> packages
        }
        val result = buildJsonObject {
            putJsonArray("apps") {
                filtered.forEach { pkg ->
                    addJsonObject {
                        put("package_name", pkg.packageName)
                        put("version_name", pkg.versionName ?: "")
                        put("is_system", (pkg.applicationInfo.flags and android.content.pm.ApplicationInfo.FLAG_SYSTEM) != 0)
                    }
                }
            }
            put("count", filtered.size)
        }
        return textResult(result.toString())
    }

    // =====================================================================
    // WAIT TOOLS
    // =====================================================================

    private suspend fun handleWaitForElement(args: JsonObject): McpToolCallResult {
        val text = args["text"]?.jsonPrimitive?.contentOrNull
        val resourceId = args["resource_id"]?.jsonPrimitive?.contentOrNull
        val contentDesc = args["content_desc"]?.jsonPrimitive?.contentOrNull
        // Clamped server-side regardless of what the schema documents: an
        // uncapped, caller-chosen poll window is how a gated tool outlives
        // the gate.
        val timeoutMs = (args["timeout_ms"]?.jsonPrimitive?.longOrNull ?: 5000L)
            .coerceIn(100L, MAX_WAIT_TIMEOUT_MS)

        val deadline = System.currentTimeMillis() + timeoutMs
        while (System.currentTimeMillis() < deadline) {
            // Re-gate every iteration: a protected app foregrounded mid-wait
            // must end the wait, not be read by it.
            activeProtectionRefusal()?.let { return it }
            val rootNode = service.rootInActiveWindow
            if (rootNode != null) {
                val found = try {
                    resolveSelector(uiTreeWalker.walkTree(rootNode), text, resourceId, contentDesc)
                } finally {
                    @Suppress("DEPRECATION")
                    rootNode.recycle()
                }
                if (found != null) {
                    return textResult("{\"found\":true,\"element\":{\"text\":\"${found.text}\",\"bounds\":\"${found.bounds}\"}}")
                }
            }
            delay(POLL_INTERVAL_MS)
        }
        return textResult("{\"found\":false,\"timeout_ms\":$timeoutMs}")
    }

    private suspend fun handleWaitForGone(args: JsonObject): McpToolCallResult {
        val text = args["text"]?.jsonPrimitive?.contentOrNull
        val resourceId = args["resource_id"]?.jsonPrimitive?.contentOrNull
        val contentDesc = args["content_desc"]?.jsonPrimitive?.contentOrNull
        val timeoutMs = (args["timeout_ms"]?.jsonPrimitive?.longOrNull ?: 5000L)
            .coerceIn(100L, MAX_WAIT_TIMEOUT_MS)

        val deadline = System.currentTimeMillis() + timeoutMs
        while (System.currentTimeMillis() < deadline) {
            // A protected foreground ends the wait: even the presence/absence
            // bit is an oracle against a screen the agent may not read.
            activeProtectionRefusal()?.let { return it }
            val rootNode = service.rootInActiveWindow
            if (rootNode == null) return textResult("{\"found\":false}")
            val found = try {
                resolveSelector(uiTreeWalker.walkTree(rootNode), text, resourceId, contentDesc)
            } finally {
                @Suppress("DEPRECATION")
                rootNode.recycle()
            }
            if (found == null) return textResult("{\"found\":false}")
            delay(POLL_INTERVAL_MS)
        }
        return textResult("{\"found\":true,\"note\":\"Element still present after ${timeoutMs}ms\"}")
    }

    private suspend fun handleWaitForIdle(args: JsonObject): McpToolCallResult {
        val timeoutMs = (args["timeout_ms"]?.jsonPrimitive?.longOrNull ?: 5000L)
            .coerceIn(100L, MAX_WAIT_TIMEOUT_MS)
        var lastEventTime = System.currentTimeMillis()
        val idleThreshold = 500L

        val listener = object : AccessibilityEventListener {
            override fun onEvent(event: android.view.accessibility.AccessibilityEvent) {
                lastEventTime = System.currentTimeMillis()
            }
        }
        service.registerEventListener(listener)
        return try {
            withTimeoutOrNull(timeoutMs) {
                while (true) {
                    delay(idleThreshold)
                    if (System.currentTimeMillis() - lastEventTime >= idleThreshold) break
                }
            }
            textResult("{\"idle\":true}")
        } finally {
            // Always unregister — covers normal exit, timeout, and coroutine cancellation
            service.unregisterEventListener(listener)
        }
    }

    private suspend fun handleScrollToElement(args: JsonObject): McpToolCallResult {
        val text = args["text"]?.jsonPrimitive?.contentOrNull
        val resourceId = args["resource_id"]?.jsonPrimitive?.contentOrNull
        val contentDesc = args["content_desc"]?.jsonPrimitive?.contentOrNull
        val direction = args["direction"]?.jsonPrimitive?.contentOrNull ?: "down"
        val maxScrolls = (args["max_scrolls"]?.jsonPrimitive?.intOrNull ?: 20)
            .coerceIn(1, MAX_SCROLL_ATTEMPTS)

        val dm = service.resources.displayMetrics
        val centerX = dm.widthPixels / 2f
        val startY = if (direction == "down") dm.heightPixels * 0.7f else dm.heightPixels * 0.3f
        val endY = if (direction == "down") dm.heightPixels * 0.3f else dm.heightPixels * 0.7f

        repeat(maxScrolls) { scrollCount ->
            // Re-gate before every read AND every swipe: this handler does
            // not just observe mid-loop, it acts — a protected app that
            // arrives between iterations must not be scrolled.
            activeProtectionRefusal()?.let { return it }
            val rootNode = service.rootInActiveWindow
            if (rootNode != null) {
                val found = try {
                    resolveSelector(uiTreeWalker.walkTree(rootNode), text, resourceId, contentDesc)
                } finally {
                    @Suppress("DEPRECATION")
                    rootNode.recycle()
                }
                if (found != null) return textResult("{\"found\":true,\"scrolls\":$scrollCount}")
            }
            withTimeoutOrNull(1000L) {
                executeGestureAndWait { cb -> gestureEngine.executeSwipe(centerX, startY, centerX, endY, 300L, cb) }
            }
            delay(200)
        }
        return textResult("{\"found\":false,\"scrolls\":$maxScrolls}")
    }

    // =====================================================================
    // DEVICE / META / TEST TOOLS
    // =====================================================================

    private fun handleListDevices(): McpToolCallResult {
        val dm = service.resources.displayMetrics
        val result = buildJsonObject {
            putJsonArray("devices") {
                addJsonObject {
                    put("device_id", "local")
                    put("model", "${Build.MANUFACTURER} ${Build.MODEL}")
                    put("android_version", Build.VERSION.RELEASE)
                    put("sdk_int", Build.VERSION.SDK_INT)
                    put("screen_width", dm.widthPixels)
                    put("screen_height", dm.heightPixels)
                    put("status", "connected")
                    put("all_permissions_ready", true)
                }
            }
        }
        return textResult(result.toString())
    }

    private fun handleSearchTools(args: JsonObject): McpToolCallResult {
        val query = args["query"]?.jsonPrimitive?.contentOrNull ?: return errorResult("query required")
        val category = args["category"]?.jsonPrimitive?.contentOrNull
        val allTools = McpToolRegistry.getAllTools()
        val matches = allTools.filter { tool ->
            (tool.name.contains(query, ignoreCase = true) || tool.description.contains(query, ignoreCase = true)) &&
            (category == null || tool.name.contains(category, ignoreCase = true))
        }
        val result = buildJsonObject {
            putJsonArray("tools") {
                matches.forEach { t ->
                    addJsonObject {
                        put("name", t.name)
                        put("description", t.description)
                    }
                }
            }
            put("count", matches.size)
        }
        return textResult(result.toString())
    }

    private fun handleDescribeTools(args: JsonObject): McpToolCallResult {
        val toolNames = args["tools"]?.jsonArray?.mapNotNull { it.jsonPrimitive.contentOrNull } ?: emptyList()
        val result = buildJsonObject {
            putJsonArray("tools") {
                toolNames.forEach { name ->
                    val tool = McpToolRegistry.getTool(name)
                    if (tool != null) {
                        addJsonObject {
                            put("name", tool.name)
                            put("description", tool.description)
                            put("inputSchema", tool.inputSchema)
                        }
                    }
                }
            }
        }
        return textResult(result.toString())
    }

    private fun handleEnableEvents(args: JsonObject): McpToolCallResult {
        val enable = args["enable"]?.jsonPrimitive?.booleanOrNull ?: return errorResult("enable (boolean) required")
        service.setEventsEnabled(enable)
        return textResult("{\"events_enabled\":$enable}")
    }

    private fun handleGetDeviceInfo(): McpToolCallResult {
        val dm = service.resources.displayMetrics
        val result = buildJsonObject {
            put("manufacturer", Build.MANUFACTURER)
            put("model", Build.MODEL)
            put("android_version", Build.VERSION.RELEASE)
            put("sdk_int", Build.VERSION.SDK_INT)
            put("screen_width", dm.widthPixels)
            put("screen_height", dm.heightPixels)
            put("density_dpi", dm.densityDpi)
            put("density", dm.density)
        }
        return textResult(result.toString())
    }

}
