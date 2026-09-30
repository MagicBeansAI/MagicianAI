package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.protection.ProtectedApps
import android.content.Intent
import android.content.pm.PackageManager
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle

/**
 * The owner's list of apps an agent may not look inside.
 *
 * While one of these is on screen, the companion refuses snapshot, screenshot
 * and every gesture; its notifications and toasts never leave the device, and
 * a one-time code from it can never be awaited. Seeded with authenticator
 * apps; the owner's edits are final — an emptied list stays empty.
 */
@Composable
fun ProtectedAppsSettingsPage() {
    val context = LocalContext.current
    // The process-wide instance — the same one the live bridge gate consults,
    // so a toggle here applies to the very next tool call.
    val store = remember(context) { ProtectedApps.get(context) }
    val protected by store.packages.collectAsStateWithLifecycle()

    val installed = remember(context) {
        val pm = context.packageManager
        val launchable = pm
            .queryIntentActivities(
                Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER),
                PackageManager.MATCH_ALL,
            )
            .map { it.activityInfo.packageName }
            .toSortedSet()
        launchable.map { pkg ->
            val label = runCatching {
                pm.getApplicationLabel(pm.getApplicationInfo(pkg, 0)).toString()
            }.getOrDefault(pkg)
            pkg to label
        }
    }

    // Seeded-but-not-installed packages still matter: the owner should see
    // that an authenticator would be protected the day it is installed, and
    // be able to unprotect it in advance if they disagree.
    val uninstalledProtected = (protected - installed.map { it.first }.toSet()).sorted()

    Column(Modifier.fillMaxSize().padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        Text(
            "While a protected app is on screen, the assistant cannot see it, " +
                "screenshot it, or touch it — and its notifications stay on this phone.",
            color = Secondary,
            fontSize = 13.sp,
        )
        LazyColumn(Modifier.fillMaxSize()) {
            items(installed, key = { it.first }) { (pkg, label) ->
                ProtectedAppRow(
                    label = label,
                    packageName = pkg,
                    protected = pkg in protected,
                    onToggle = { store.setProtected(pkg, pkg !in protected) },
                )
                HorizontalDivider(color = BorderSoft, modifier = Modifier.padding(start = 12.dp))
            }
            items(uninstalledProtected, key = { "ghost-$it" }) { pkg ->
                ProtectedAppRow(
                    label = pkg,
                    packageName = "protected when installed",
                    protected = true,
                    onToggle = { store.setProtected(pkg, false) },
                )
                HorizontalDivider(color = BorderSoft, modifier = Modifier.padding(start = 12.dp))
            }
        }
    }
}

@Composable
private fun ProtectedAppRow(
    label: String,
    packageName: String,
    protected: Boolean,
    onToggle: () -> Unit,
) {
    Row(
        Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 8.dp),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f)) {
            Text(label, color = Ink, fontSize = 14.sp, fontWeight = FontWeight.Medium)
            Text(packageName, color = Muted, fontSize = 11.sp)
        }
        Switch(checked = protected, onCheckedChange = { onToggle() })
    }
}
