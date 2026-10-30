package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.preferences.UiPreferencesClient
import ai.magicbeans.magdroid.preferences.UiThemePreferences
import android.content.Context
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch

/**
 * The theme follows the account. `/ui/preferences` already carries the web's
 * theme choice; this joins the app to the same contract, so a theme picked on
 * the web, the iPhone or here shows everywhere:
 *
 * - On resume, the stored preference is adopted when one exists (the account
 *   is the truth, as the web already treats it); when none exists, the local
 *   choice seeds the account — again what the web does.
 * - An explicit pick here (the Themes sheet) is pushed to the account at once.
 *
 * Everything is best-effort: unpaired, offline or a failing call leaves the
 * local [ThemeStore] fully in charge, exactly as before this sync existed.
 */
object ThemeSync {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    /** Read the account's choice and converge on it. The account is the truth
     * — the web already treats it that way — with one first-encounter rule so
     * an established local pick is not silently overwritten by a web-seeded
     * default: the FIRST time this install meets the account it declares the
     * local choice, unless the account holds a deliberate non-default theme.
     * After that, the stored choice always wins. */
    fun reconcileAsync(context: Context, systemIsDark: Boolean) {
        scope.launch {
            val app = context.applicationContext
            val store = ThemeStore.get(app)
            val client = UiPreferencesClient(app)
            try {
                val preferences = client.fetch()
                if (!preferences.saved) {
                    // No stored choice: this surface's local choice seeds the
                    // account, exactly as the web seeds it.
                    pushNow(client, store, systemIsDark)
                    return@launch
                }
                if (!store.accountSeeded) {
                    store.markAccountSeeded()
                    val deliberate = preferences.theme != Themes.DEFAULT ||
                        preferences.appearance != null
                    if (!deliberate) {
                        // A web-seeded default, not a choice made on purpose:
                        // this install's theme becomes the account's once.
                        pushNow(client, store, systemIsDark)
                        return@launch
                    }
                }
                adopt(store, preferences, systemIsDark)
            } catch (_: Exception) {
                // Unpaired or offline — the local store keeps working alone.
            } finally {
                client.close()
            }
        }
    }

    /** Push an explicit local pick to the account. Fire-and-forget: a failed
     * push keeps the local choice, and the next successful change converges. */
    fun pushAsync(context: Context, systemIsDark: Boolean) {
        scope.launch {
            val app = context.applicationContext
            val client = UiPreferencesClient(app)
            try {
                pushNow(client, ThemeStore.get(app), systemIsDark)
            } catch (_: Exception) {
            } finally {
                client.close()
            }
        }
    }

    private suspend fun pushNow(
        client: UiPreferencesClient,
        store: ThemeStore,
        systemIsDark: Boolean,
    ) {
        val familyId = store.familyId.value
        val mode = store.mode.value
        client.push(
            theme = Themes.resolve(familyId, mode, systemIsDark),
            appearance = mode.name.lowercase(),
        )
    }

    /** Apply the stored variant (and mode) when they differ from local. An
     * unknown id leaves the local choice alone; adoption never pushes, so
     * converging cannot echo back. */
    private fun adopt(store: ThemeStore, preferences: UiThemePreferences, systemIsDark: Boolean) {
        val id = Themes.migratedId(preferences.theme)
        val family = Themes.families.firstOrNull { it.id == id || it.dark == id } ?: return
        val mode = when (preferences.appearance) {
            "system" -> AppearanceMode.System
            "day" -> AppearanceMode.Day
            "night" -> AppearanceMode.Night
            // The account carries no mode (a web write): the variant's own
            // side stands in, so the family arrives looking as it was picked.
            else -> if (preferences.theme == family.dark) AppearanceMode.Night else AppearanceMode.Day
        }
        if (family.id == store.familyId.value && mode == store.mode.value) return
        store.setFamily(family.id)
        store.setMode(mode)
        applyTheme(family.id, mode, systemIsDark)
    }
}
