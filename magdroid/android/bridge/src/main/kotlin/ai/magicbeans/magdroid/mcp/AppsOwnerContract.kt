package ai.magicbeans.magdroid.mcp

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.put
import java.security.MessageDigest

internal const val APPS_OWNER_SCHEMA = "magician.android-app-owner.v1"
internal const val APPS_OWNER_ARGUMENT = "_magician_apps"
internal const val MAX_APPS_OWNER_RESULT_BYTES = 8 * 1024 * 1024
internal const val MAX_APPS_OWNER_SCREENSHOT_BYTES = 4 * 1024 * 1024
internal const val MAX_APPS_OWNER_NODES = 4096
internal const val MAX_APPS_OWNER_PACKAGES = 128

internal enum class AppsOwnerAction(val wireName: String) {
    Snapshot("snapshot"),
    Screenshot("screenshot"),
    Launch("launch"),
    Close("close"),
    Tap("tap"),
    Type("type"),
    Key("key"),
    Scroll("scroll"),
}

internal data class AppsOwnerObservation(
    val foregroundPackage: String,
    val snapshotSha256: String,
    val width: Int,
    val height: Int,
)

internal data class AppsOwnerClaim(
    val permitNonce: String,
    val action: AppsOwnerAction,
    val allowedPackages: Set<String>,
    val targetPackage: String?,
    val observation: AppsOwnerObservation?,
    val maxResultBytes: Int,
    val maxEvidenceNodes: Int,
)

internal val appsOwnerToolActions: Map<String, AppsOwnerAction> = linkedMapOf(
    "android_get_ui_tree" to AppsOwnerAction.Snapshot,
    "android_screenshot" to AppsOwnerAction.Screenshot,
    "android_launch_app" to AppsOwnerAction.Launch,
    "android_close_app" to AppsOwnerAction.Close,
    "android_tap" to AppsOwnerAction.Tap,
    "android_input_text" to AppsOwnerAction.Type,
    "android_press_key" to AppsOwnerAction.Key,
    "android_swipe" to AppsOwnerAction.Scroll,
)

internal fun appsOwnerResultReceipt(
    claim: AppsOwnerClaim,
    exactPayload: ByteArray,
    foregroundPackage: String?,
    snapshotSha256: String?,
    width: Int?,
    height: Int?,
    evidenceNodes: Int,
    truncated: Boolean,
): JsonObject {
    require(exactPayload.size <= claim.maxResultBytes) {
        "Apps owner result exceeds admitted byte ceiling"
    }
    require(evidenceNodes in 0..claim.maxEvidenceNodes) {
        "Apps owner result exceeds admitted node ceiling"
    }
    return buildJsonObject {
        put("schema", APPS_OWNER_SCHEMA)
        put("permit_nonce", claim.permitNonce)
        put("action", claim.action.wireName)
        put("target_package", claim.targetPackage?.let(::JsonPrimitive) ?: JsonNull)
        put("foreground_package", foregroundPackage?.let(::JsonPrimitive) ?: JsonNull)
        put("snapshot_sha256", snapshotSha256?.let(::JsonPrimitive) ?: JsonNull)
        put("width", width?.let(::JsonPrimitive) ?: JsonNull)
        put("height", height?.let(::JsonPrimitive) ?: JsonNull)
        put("evidence_bytes", exactPayload.size)
        put("evidence_nodes", evidenceNodes)
        put("truncated", truncated)
        put("outcome", "settled")
        put("result_sha256", appsOwnerSha256(exactPayload))
    }
}

internal fun appsOwnerSha256(bytes: ByteArray): String = MessageDigest.getInstance("SHA-256")
    .digest(bytes)
    .joinToString("") { "%02x".format(it.toInt() and 0xff) }

/**
 * Parse the private Apps claim and the closed arguments it accompanies.
 * Ambient callers may omit the claim; once present, no other tool or legacy
 * selector/option is accepted.
 */
internal fun parseAppsOwnerClaim(toolName: String, args: JsonObject): AppsOwnerClaim? {
    val raw = args[APPS_OWNER_ARGUMENT] ?: return null
    val expectedAction = appsOwnerToolActions[toolName]
        ?: throw IllegalArgumentException("Apps owner claim is not valid for this tool")
    val value = raw as? JsonObject ?: throw IllegalArgumentException("invalid Apps owner claim")
    require(
        value.keys == setOf(
            "schema",
            "permit_nonce",
            "action",
            "allowed_packages",
            "target_package",
            "observation",
            "max_result_bytes",
            "max_evidence_nodes",
        ),
    ) { "Apps owner claim fields are not exact" }
    require(value.strictString("schema") == APPS_OWNER_SCHEMA) {
        "invalid Apps owner claim schema"
    }
    val nonce = value.strictString("permit_nonce")
    require(nonce.length in 16..128 && nonce.all { it.isLetterOrDigit() || it in "-_" }) {
        "invalid Apps owner permit nonce"
    }
    val actionName = value.strictString("action")
    require(actionName == expectedAction.wireName) { "Apps owner action does not match tool" }

    val packageValues = value["allowed_packages"] as? JsonArray
        ?: throw IllegalArgumentException("Apps owner package set is required")
    val packages = packageValues.map { element ->
        val primitive = element as? JsonPrimitive
            ?: throw IllegalArgumentException("invalid Apps owner package")
        require(primitive.isString) { "invalid Apps owner package" }
        primitive.content
    }
    require(
        packages.size in 1..MAX_APPS_OWNER_PACKAGES &&
            packages.toSet().size == packages.size &&
            packages.zipWithNext().all { (left, right) -> left < right } &&
            packages.all(::validAppsPackageName),
    ) { "invalid Apps owner package set" }

    val target = when (val rawTarget = value["target_package"]) {
        JsonNull -> null
        is JsonPrimitive -> rawTarget.takeIf { it.isString }?.contentOrNull
            ?: throw IllegalArgumentException("invalid Apps owner target package")
        else -> throw IllegalArgumentException("Apps owner target package is required")
    }
    require(target == null || target in packages) {
        "Apps owner target package is outside the reviewed set"
    }

    val observation = when (val rawObservation = value["observation"]) {
        JsonNull -> null
        is JsonObject -> parseAppsOwnerObservation(rawObservation, packages, target)
        else -> throw IllegalArgumentException("invalid Apps owner observation")
    }
    val observationRequired = expectedAction in setOf(
        AppsOwnerAction.Tap,
        AppsOwnerAction.Type,
        AppsOwnerAction.Key,
        AppsOwnerAction.Scroll,
    )
    require(observationRequired == (observation != null)) {
        "Apps owner observation presence does not match action"
    }
    require((expectedAction == AppsOwnerAction.Snapshot) == (target == null)) {
        "Apps owner target presence does not match action"
    }

    val maxResultBytes = value.strictInt("max_result_bytes")
    val maxEvidenceNodes = value.strictInt("max_evidence_nodes")
    require(maxResultBytes in 1..MAX_APPS_OWNER_RESULT_BYTES) {
        "invalid Apps owner result ceiling"
    }
    require(maxEvidenceNodes in 1..MAX_APPS_OWNER_NODES) {
        "invalid Apps owner node ceiling"
    }

    val claim = AppsOwnerClaim(
        permitNonce = nonce,
        action = expectedAction,
        allowedPackages = packages.toSet(),
        targetPackage = target,
        observation = observation,
        maxResultBytes = maxResultBytes,
        maxEvidenceNodes = maxEvidenceNodes,
    )
    validateAppsOwnerArguments(args, claim)
    return claim
}

private fun parseAppsOwnerObservation(
    value: JsonObject,
    packages: List<String>,
    target: String?,
): AppsOwnerObservation {
    require(
        value.keys == setOf("foreground_package", "snapshot_sha256", "width", "height"),
    ) { "Apps owner observation fields are not exact" }
    val foreground = value.strictString("foreground_package")
    val digest = value.strictString("snapshot_sha256")
    val width = value.strictInt("width")
    val height = value.strictInt("height")
    require(foreground == target && foreground in packages) {
        "Apps owner observation package does not match target"
    }
    require(digest.length == 64 && digest.all { it in '0'..'9' || it in 'a'..'f' }) {
        "invalid Apps owner snapshot digest"
    }
    require(width > 0 && height > 0) { "invalid Apps owner observation geometry" }
    return AppsOwnerObservation(foreground, digest, width, height)
}

private fun validateAppsOwnerArguments(args: JsonObject, claim: AppsOwnerClaim) {
    fun exact(vararg names: String) {
        require(args.keys == names.toSet() + APPS_OWNER_ARGUMENT) {
            "Apps owner arguments are not exact for ${claim.action.wireName}"
        }
    }

    when (claim.action) {
        AppsOwnerAction.Snapshot -> {
            exact("filter", "max_depth")
            require(args.strictString("filter") == "interactive" && args.strictInt("max_depth") == 50) {
                "invalid Apps snapshot arguments"
            }
        }
        AppsOwnerAction.Screenshot -> {
            exact("quality")
            require(args.strictString("quality") == "full") { "invalid Apps screenshot quality" }
        }
        AppsOwnerAction.Launch -> {
            exact("package_name", "clear_task")
            require(
                args.strictString("package_name") == claim.targetPackage &&
                    args.strictBoolean("clear_task") == false,
            ) { "invalid Apps launch arguments" }
        }
        AppsOwnerAction.Close -> {
            exact("package_name", "force")
            require(
                args.strictString("package_name") == claim.targetPackage &&
                    args.strictBoolean("force") == false,
            ) { "invalid Apps close arguments" }
        }
        AppsOwnerAction.Tap -> {
            exact("x", "y")
            val observation = requireNotNull(claim.observation)
            require(
                args.strictInt("x") in 0 until observation.width &&
                    args.strictInt("y") in 0 until observation.height,
            ) { "Apps tap coordinates are outside reviewed geometry" }
        }
        AppsOwnerAction.Type -> {
            exact("text", "append")
            require(
                args.strictString("text").length in 1..16_384 &&
                    args.strictBoolean("append") == false,
            ) { "invalid Apps type arguments" }
        }
        AppsOwnerAction.Key -> {
            exact("key")
            require(
                args.strictString("key") in
                    setOf("back", "home", "enter", "delete", "tab", "escape", "space"),
            ) { "invalid Apps key" }
        }
        AppsOwnerAction.Scroll -> {
            exact("start_x", "start_y", "end_x", "end_y", "duration_ms")
            val observation = requireNotNull(claim.observation)
            require(
                args.strictInt("start_x") in 0 until observation.width &&
                    args.strictInt("end_x") in 0 until observation.width &&
                    args.strictInt("start_y") in 0 until observation.height &&
                    args.strictInt("end_y") in 0 until observation.height &&
                    args.strictInt("duration_ms") in 100..900,
            ) { "invalid Apps scroll arguments" }
        }
    }
}

private fun JsonObject.strictString(name: String): String {
    val primitive = this[name] as? JsonPrimitive
        ?: throw IllegalArgumentException("$name must be a string")
    require(primitive.isString) { "$name must be a string" }
    return primitive.content
}

private fun JsonObject.strictInt(name: String): Int {
    val primitive = this[name] as? JsonPrimitive
        ?: throw IllegalArgumentException("$name must be an integer")
    require(!primitive.isString) { "$name must be an integer" }
    return primitive.intOrNull ?: throw IllegalArgumentException("$name must be an integer")
}

private fun JsonObject.strictBoolean(name: String): Boolean {
    val primitive = this[name] as? JsonPrimitive
        ?: throw IllegalArgumentException("$name must be a boolean")
    require(!primitive.isString) { "$name must be a boolean" }
    return primitive.booleanOrNull ?: throw IllegalArgumentException("$name must be a boolean")
}

private fun validAppsPackageName(value: String): Boolean =
    value.length in 3..255 &&
        value.contains('.') &&
        value.split('.').all { component ->
            component.isNotEmpty() &&
                (component.first() in 'A'..'Z' || component.first() in 'a'..'z') &&
                component.all {
                    it in 'A'..'Z' || it in 'a'..'z' || it in '0'..'9' || it == '_'
                }
        }
