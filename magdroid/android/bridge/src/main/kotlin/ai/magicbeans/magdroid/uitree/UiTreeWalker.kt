package ai.magicbeans.magdroid.uitree

import android.accessibilityservice.AccessibilityService
import android.graphics.Rect
import android.util.Log
import android.view.accessibility.AccessibilityNodeInfo
import ai.magicbeans.magdroid.service.Bounds
import ai.magicbeans.magdroid.service.UiElement
import ai.magicbeans.magdroid.service.UiTree
import java.security.MessageDigest

/**
 * Redact secure accessibility values before they can influence element IDs,
 * descriptions, semantic hashes, or MCP results. A password node's geometry
 * and structural interaction flags remain useful, but its text is never Apps
 * evidence even when an application populates `contentDescription` with it.
 */
internal fun accessibilityTextFields(
    redactedAncestor: Boolean,
    isPassword: Boolean,
    isEditable: Boolean,
    text: CharSequence?,
    contentDescription: CharSequence?
): Pair<String, String> = if (redactedAncestor || isPassword || isEditable) {
    "" to ""
} else {
    (text?.toString() ?: "") to (contentDescription?.toString() ?: "")
}

internal fun boundedAccessibilityUtf8(
    value: CharSequence?,
    maximumBytes: Int
): String {
    if (value == null || maximumBytes <= 0) return ""
    val bounded = StringBuilder(minOf(value.length, maximumBytes))
    var used = 0
    var index = 0
    while (index < value.length) {
        val first = value[index]
        val codePoint = if (Character.isHighSurrogate(first) &&
            index + 1 < value.length && Character.isLowSurrogate(value[index + 1])) {
            Character.toCodePoint(first, value[index + 1])
        } else {
            first.code
        }
        val projectedCodePoint = when (codePoint) {
            '\n'.code, '\r'.code -> ' '.code
            '|'.code -> '¦'.code
            else -> codePoint
        }
        val width = when {
            projectedCodePoint <= 0x7f -> 1
            projectedCodePoint <= 0x7ff -> 2
            projectedCodePoint <= 0xffff -> 3
            else -> 4
        }
        if (used + width > maximumBytes) break
        bounded.appendCodePoint(projectedCodePoint)
        used += width
        index += if (codePoint > 0xffff) 2 else 1
    }
    return bounded.toString()
}

internal fun accessibilityNodeBelongsToPackage(
    nodePackage: CharSequence?,
    expectedPackage: String
): Boolean = nodePackage != null && nodePackage.contentEquals(expectedPackage)

internal data class AppsVisibleBounds(
    val left: Int,
    val top: Int,
    val right: Int,
    val bottom: Int,
)

/** Intersect OEM/transient accessibility geometry with the reviewed display. */
internal fun intersectAccessibilityBounds(
    left: Int,
    top: Int,
    right: Int,
    bottom: Int,
    displayWidth: Int,
    displayHeight: Int,
): AppsVisibleBounds? {
    if (displayWidth <= 0 || displayHeight <= 0 || right <= left || bottom <= top) return null
    val visibleLeft = left.coerceIn(0, displayWidth)
    val visibleTop = top.coerceIn(0, displayHeight)
    val visibleRight = right.coerceIn(0, displayWidth)
    val visibleBottom = bottom.coerceIn(0, displayHeight)
    if (visibleRight <= visibleLeft || visibleBottom <= visibleTop) return null
    return AppsVisibleBounds(visibleLeft, visibleTop, visibleRight, visibleBottom)
}

data class AppsUiProjection(
    val table: String,
    val totalNodes: Int,
    val shownNodes: Int,
    val truncated: Boolean,
    val foreignPackageDetected: Boolean
)

/**
 * UI Tree Walker
 *
 * Walks the accessibility node tree and extracts semantic information
 * optimized for AI agent consumption.
 *
 * Features:
 * - Generates stable element IDs (hash-based)
 * - Classifies elements into semantic types (button, input, text, etc.)
 * - Creates human-readable AI descriptions
 * - Filters invisible/unimportant elements
 * - Maintains parent-child relationships
 */
class UiTreeWalker(
    private val accessibilityService: AccessibilityService
) {
    companion object {
        private const val TAG = "UiTreeWalker"
        private const val MAX_RECURSION_DEPTH = 50
        private const val MAX_TREE_NODES = 4096
        private const val MAX_APPS_FIELD_BYTES = 512
        private const val APPS_TABLE_HEADER = "IDX | text | desc | flags | bounds\n"

        // Semantic type classifications
        private val BUTTON_CLASSES = setOf(
            "android.widget.Button",
            "android.widget.ImageButton",
            "androidx.appcompat.widget.AppCompatButton",
            "com.google.android.material.button.MaterialButton"
        )

        private val INPUT_CLASSES = setOf(
            "android.widget.EditText",
            "androidx.appcompat.widget.AppCompatEditText",
            "com.google.android.material.textfield.TextInputEditText"
        )

        private val TEXT_CLASSES = setOf(
            "android.widget.TextView",
            "android.widget.TextClock",
            "androidx.appcompat.widget.AppCompatTextView"
        )

        private val IMAGE_CLASSES = setOf(
            "android.widget.ImageView",
            "androidx.appcompat.widget.AppCompatImageView"
        )

        private val LIST_CLASSES = setOf(
            "android.widget.ListView",
            "android.widget.RecyclerView",
            "androidx.recyclerview.widget.RecyclerView"
        )

        private val SCROLL_CLASSES = setOf(
            "android.widget.ScrollView",
            "android.widget.HorizontalScrollView",
            "androidx.core.widget.NestedScrollView"
        )
    }

    /**
     * Build the Apps-only projection directly under its admitted byte budget.
     * It never allocates the generic UiElement tree and never converts an
     * unbounded accessibility CharSequence into a String. Foreign-package
     * overlays are detected per node and excluded with their whole subtree.
     */
    suspend fun walkAppsProjection(
        rootNode: AccessibilityNodeInfo,
        expectedPackage: String,
        displayWidth: Int,
        displayHeight: Int,
        maxDepth: Int,
        maxNodes: Int,
        maxTableBytes: Int
    ): AppsUiProjection {
        val table = StringBuilder(minOf(maxTableBytes.coerceAtLeast(0), 64 * 1024))
        var tableBytes = 0
        var totalNodes = 0
        var shownNodes = 0
        var truncated = false
        var foreignPackageDetected = false
        var halted = false
        val nodeCeiling = maxNodes.coerceIn(1, MAX_TREE_NODES)
        val depthCeiling = if (maxDepth <= 0) MAX_RECURSION_DEPTH else {
            maxDepth.coerceIn(1, MAX_RECURSION_DEPTH)
        }

        fun appendBounded(value: String): Boolean {
            val bytes = value.toByteArray(Charsets.UTF_8).size
            if (bytes > maxTableBytes - tableBytes) return false
            table.append(value)
            tableBytes += bytes
            return true
        }

        if (!appendBounded(APPS_TABLE_HEADER)) {
            return AppsUiProjection("", 0, 0, true, false)
        }

        fun visit(node: AccessibilityNodeInfo, depth: Int, redactedAncestor: Boolean) {
            if (halted) return
            if (totalNodes >= nodeCeiling || depth >= depthCeiling) {
                truncated = true
                halted = totalNodes >= nodeCeiling
                return
            }
            totalNodes += 1
            val packageName = node.packageName
            if (!accessibilityNodeBelongsToPackage(packageName, expectedPackage)) {
                foreignPackageDetected = true
                truncated = true
                return
            }
            if (!node.isVisibleToUser) return
            val redactSubtree = redactedAncestor || node.isPassword || node.isEditable
            val text = if (redactSubtree) "" else {
                boundedAccessibilityUtf8(node.text, MAX_APPS_FIELD_BYTES)
            }
            val description = if (redactSubtree) "" else {
                boundedAccessibilityUtf8(node.contentDescription, MAX_APPS_FIELD_BYTES)
            }
            val include = node.isClickable || node.isFocusable || node.isScrollable ||
                node.isCheckable || text.isNotEmpty() || description.isNotEmpty()
            if (include) {
                val flags = buildString(4) {
                    if (node.isClickable) append('c')
                    if (node.isFocusable) append('f')
                    if (node.isScrollable) append('s')
                    if (node.isCheckable) append('k')
                }
                val bounds = Rect()
                node.getBoundsInScreen(bounds)
                val visibleBounds = intersectAccessibilityBounds(
                    bounds.left,
                    bounds.top,
                    bounds.right,
                    bounds.bottom,
                    displayWidth,
                    displayHeight,
                )
                if (visibleBounds == null) {
                    truncated = true
                    return
                }
                val row = "$shownNodes | $text | $description | $flags | " +
                    "[${visibleBounds.left},${visibleBounds.top},${visibleBounds.right},${visibleBounds.bottom}]\n"
                if (!appendBounded(row)) {
                    truncated = true
                    halted = true
                    return
                }
                shownNodes += 1
            }
            for (index in 0 until node.childCount) {
                if (halted) break
                val child = node.getChild(index) ?: continue
                try {
                    visit(child, depth + 1, redactSubtree)
                } finally {
                    @Suppress("DEPRECATION")
                    child.recycle()
                }
            }
        }

        visit(rootNode, 0, false)
        return AppsUiProjection(
            table = table.toString().trimEnd(),
            totalNodes = totalNodes,
            shownNodes = shownNodes,
            truncated = truncated,
            foreignPackageDetected = foreignPackageDetected
        )
    }

    /**
     * Walk the UI tree and extract all elements
     *
     * @param rootNode Root accessibility node (typically from rootInActiveWindow)
     * @param includeInvisible Include invisible elements
     * @param maxDepth Maximum tree depth (0 = unlimited)
     * @return UI tree with all elements
     */
    suspend fun walkTree(
        rootNode: AccessibilityNodeInfo?,
        includeInvisible: Boolean = false,
        maxDepth: Int = 0,
        maxNodes: Int = MAX_TREE_NODES
    ): UiTree {
        val startTime = System.currentTimeMillis()
        val elements = mutableListOf<UiElement>()
        var totalNodes = 0
        var truncated = false
        val nodeCeiling = maxNodes.coerceIn(1, MAX_TREE_NODES)

        if (rootNode == null) {
            Log.w(TAG, "Root node is null, returning empty tree")
            return UiTree(
                elements = emptyList(),
                foregroundApp = "unknown",
                totalNodes = 0,
                captureTimestamp = startTime
            )
        }

        // Get foreground app package
        val foregroundApp = rootNode.packageName?.toString() ?: "unknown"

        Log.d(TAG, "Walking UI tree for app: $foregroundApp")

        // Recursive tree walk
        walkNode(
            node = rootNode,
            parentId = null,
            depth = 0,
            maxDepth = maxDepth,
            includeInvisible = includeInvisible,
            elements = elements,
            redactedAncestor = false,
            takeNode = {
                if (totalNodes >= nodeCeiling) {
                    truncated = true
                    false
                } else {
                    totalNodes++
                    true
                }
            }
        )

        val elapsedMs = System.currentTimeMillis() - startTime
        Log.i(TAG, "UI tree walk complete: ${elements.size} elements, $totalNodes nodes, ${elapsedMs}ms")

        return UiTree(
            elements = elements,
            foregroundApp = foregroundApp,
            totalNodes = totalNodes,
            captureTimestamp = startTime,
            truncated = truncated
        )
    }

    /**
     * Recursively walk node tree
     */
    private fun walkNode(
        node: AccessibilityNodeInfo,
        parentId: String?,
        depth: Int,
        maxDepth: Int,
        includeInvisible: Boolean,
        elements: MutableList<UiElement>,
        redactedAncestor: Boolean,
        takeNode: () -> Boolean
    ) {
        if (!takeNode()) return

        // Enforce maximum recursion depth for security
        if (depth >= MAX_RECURSION_DEPTH) {
            Log.w(TAG, "Maximum recursion depth ($MAX_RECURSION_DEPTH) reached, stopping tree walk")
            return
        }

        // Check depth limit
        if (maxDepth > 0 && depth >= maxDepth) {
            return
        }

        // Skip invisible nodes unless requested
        if (!includeInvisible && !node.isVisibleToUser) {
            return
        }

        // OEM/custom accessibility trees can put the sensitive flag on a
        // container while exposing its value in otherwise ordinary children.
        // Once a subtree is sensitive, no descendant may reintroduce text or
        // selector-bearing class/resource fields into IDs, hashes or results.
        val redactSubtree = redactedAncestor || node.isPassword || node.isEditable

        // Extract element information
        val element = extractElement(node, parentId, depth, redactSubtree)

        // Add to list
        elements.add(element)

        // Walk children
        val childCount = node.childCount
        for (i in 0 until childCount) {
            val child = node.getChild(i) ?: continue

            try {
                walkNode(
                    node = child,
                    parentId = element.elementId,
                    depth = depth + 1,
                    maxDepth = maxDepth,
                    includeInvisible = includeInvisible,
                    elements = elements,
                    redactedAncestor = redactSubtree,
                    takeNode = takeNode
                )
            } finally {
                // Note: recycle() is deprecated but still necessary for memory management
                // on API levels < 34. Safe to call on all versions.
                @Suppress("DEPRECATION")
                child.recycle()
            }
        }
    }

    /**
     * Extract element information from node
     */
    private fun extractElement(
        node: AccessibilityNodeInfo,
        parentId: String?,
        depth: Int,
        redactSubtree: Boolean
    ): UiElement {
        // Get basic properties
        val actualClassName = node.className?.toString() ?: ""
        val className = if (redactSubtree) "" else actualClassName
        val resourceId = if (redactSubtree) "" else (node.viewIdResourceName ?: "")
        val (text, contentDesc) = accessibilityTextFields(
            redactedAncestor = redactSubtree,
            isPassword = node.isPassword,
            isEditable = node.isEditable,
            text = node.text,
            contentDescription = node.contentDescription
        )

        // Get bounds
        val bounds = Rect()
        node.getBoundsInScreen(bounds)
        val elementBounds = Bounds(
            left = bounds.left,
            top = bounds.top,
            right = bounds.right,
            bottom = bounds.bottom
        )

        // Generate stable element ID
        val elementId = generateElementId(
            resourceId = resourceId,
            className = className,
            text = text,
            bounds = elementBounds,
            parentId = parentId
        )

        // Classify semantic type
        val semanticType = classifySemanticType(node, actualClassName)

        // Generate AI description
        val aiDescription = generateAiDescription(
            node = node,
            semanticType = semanticType,
            text = text,
            contentDesc = contentDesc,
            className = className
        )

        return UiElement(
            elementId = elementId,
            resourceId = resourceId.ifEmpty { null },
            className = className.ifEmpty { null },
            text = text.ifEmpty { null },
            contentDescription = contentDesc.ifEmpty { null },
            bounds = elementBounds,
            visible = node.isVisibleToUser,
            enabled = node.isEnabled,
            clickable = node.isClickable,
            scrollable = node.isScrollable,
            focusable = node.isFocusable,
            longClickable = node.isLongClickable,
            checkable = node.isCheckable,
            checked = node.isChecked,
            semanticType = semanticType,
            aiDescription = aiDescription
        )
    }

    /**
     * Generate stable element ID
     *
     * Uses hash of key properties to ensure consistency across UI tree retrievals.
     */
    private fun generateElementId(
        resourceId: String,
        className: String,
        text: String,
        bounds: Bounds,
        parentId: String?
    ): String {
        // Round bounds to reduce sensitivity to minor position changes
        val roundedBounds = Bounds(
            left = (bounds.left / 10) * 10,
            top = (bounds.top / 10) * 10,
            right = (bounds.right / 10) * 10,
            bottom = (bounds.bottom / 10) * 10
        )

        // Combine properties
        val combined = "$resourceId|$className|$text|$roundedBounds|$parentId"

        // Hash to create stable ID
        return hashString(combined)
    }

    /**
     * Hash string to create stable ID
     */
    private fun hashString(input: String): String {
        val md = MessageDigest.getInstance("MD5")
        val digest = md.digest(input.toByteArray())
        return digest.joinToString("") { "%02x".format(it) }.substring(0, 16)
    }

    /**
     * Classify element into semantic type
     */
    private fun classifySemanticType(
        node: AccessibilityNodeInfo,
        className: String
    ): String {
        return when {
            className in BUTTON_CLASSES || node.isClickable -> "button"
            className in INPUT_CLASSES || node.isEditable -> "input"
            className in TEXT_CLASSES -> "text"
            className in IMAGE_CLASSES -> "image"
            className in LIST_CLASSES -> "list"
            className in SCROLL_CLASSES || node.isScrollable -> "scroll"
            node.isCheckable -> "checkbox"
            else -> "container"
        }
    }

    /**
     * Generate human-readable AI description
     */
    private fun generateAiDescription(
        node: AccessibilityNodeInfo,
        semanticType: String,
        text: String,
        contentDesc: String,
        className: String
    ): String {
        val parts = mutableListOf<String>()

        // Add semantic type
        parts.add(semanticType.capitalize())

        // Add text or content description
        when {
            text.isNotEmpty() -> parts.add("\"$text\"")
            contentDesc.isNotEmpty() -> parts.add("\"$contentDesc\"")
        }

        // Add interaction hints
        when {
            node.isClickable -> parts.add("(clickable)")
            node.isEditable -> parts.add("(editable)")
            node.isCheckable -> parts.add(if (node.isChecked) "(checked)" else "(unchecked)")
            node.isScrollable -> parts.add("(scrollable)")
        }

        // Add state
        if (!node.isEnabled) {
            parts.add("[disabled]")
        }
        if (!node.isVisibleToUser) {
            parts.add("[hidden]")
        }

        return parts.joinToString(" ")
    }

    /**
     * Find elements matching criteria
     */
    fun findElements(
        tree: UiTree,
        text: String? = null,
        resourceId: String? = null,
        contentDesc: String? = null,
        className: String? = null,
        visibleOnly: Boolean = true
    ): List<UiElement> {
        return tree.elements.filter { element ->
            // Apply filters
            if (visibleOnly && !element.visible) return@filter false

            if (text != null && !(element.text?.contains(text, ignoreCase = true) == true)) {
                return@filter false
            }

            if (resourceId != null && !(element.resourceId?.endsWith(resourceId) == true)) {
                return@filter false
            }

            if (contentDesc != null && !(element.contentDescription?.contains(contentDesc, ignoreCase = true) == true)) {
                return@filter false
            }

            if (className != null && !(element.className?.endsWith(className) == true)) {
                return@filter false
            }

            true
        }
    }
}

/**
 * Extension: Capitalize first character
 */
private fun String.capitalize(): String {
    return if (isEmpty()) this else this[0].uppercase() + this.substring(1)
}
