package ai.magicbeans.magdroid.apps

/**
 * Scripted custom-surface support policy (plan 1.6, `custom_surfaces_v1`).
 *
 * Android renders app views natively and ships no WebView today. The V1
 * host contract (origin isolation, navigation locks, download
 * suppression, a single reviewed bridge channel) is therefore NOT
 * instantiated here: this client renders the closed unsupported notice —
 * never a degraded fallback that silently widens anything.
 *
 * When a future gate admits an Android WebView host, the constraints it
 * must satisfy are listed in the ratified design (no
 * `addJavascriptInterface` ever; bridge via
 * `androidx.webkit.WebMessageListener` restricted to the surface's
 * origin; file/content/universal-access flags all false; a
 * per-installation data-directory suffix; debugging off in release; all
 * backend access proxied). Until then this object is the single source of
 * truth: any surface marker resolved to [supported] false renders the
 * notice below.
 */
object CustomSurfaceSupport {
    /** The capability is closed on this client for V1. */
    const val supported: Boolean = false

    /** Closed unsupported-client notice, matching the kernel copy. */
    const val unsupportedNotice: String =
        "Custom surfaces are not supported on this client."

    /**
     * A package declaring scripted surface entry points renders the
     * notice; a package without the marker is unaffected (no surface
     * existed for it here before either).
     */
    fun rendersUnsupportedNotice(declaresCustomSurface: Boolean): Boolean =
        declaresCustomSurface && !supported
}
