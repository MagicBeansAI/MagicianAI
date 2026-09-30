package ai.magicbeans.magdroid.ui

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/** A deck card: panel fill, soft border, the web's `surface-card`. */
@Composable
internal fun ObserveCard(
    accent: Color = BorderSoft,
    modifier: Modifier = Modifier,
    content: @Composable ColumnScope.() -> Unit,
) {
    Surface(
        color = Panel,
        shape = RoundedCornerShape(14.dp),
        border = BorderStroke(1.dp, accent),
        modifier = modifier.fillMaxWidth(),
    ) {
        Column(
            Modifier.padding(15.dp),
            verticalArrangement = Arrangement.spacedBy(11.dp),
            content = content,
        )
    }
}

@Composable
internal fun CompactAction(
    label: String,
    icon: ImageVector,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
    emphasized: Boolean = false,
    selected: Boolean = false,
    danger: Boolean = false,
    onClick: () -> Unit,
) {
    val foreground = when {
        !enabled -> Muted
        danger -> Danger
        emphasized -> OnAccent
        selected -> Danger
        else -> Coral
    }
    val background = when {
        !enabled -> Ground
        emphasized -> Coral
        selected -> Danger.copy(alpha = 0.12f)
        else -> Ground
    }
    Surface(
        color = background,
        shape = RoundedCornerShape(8.dp),
        border = if (emphasized) null else BorderStroke(1.dp, if (danger || selected) foreground.copy(alpha = 0.4f) else BorderSoft),
        modifier = modifier.then(
            if (enabled) Modifier.clickable(role = Role.Button, onClick = onClick) else Modifier,
        ),
    ) {
        Row(
            Modifier.padding(horizontal = 10.dp, vertical = 9.dp),
            horizontalArrangement = Arrangement.Center,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Icon(icon, null, tint = foreground, modifier = Modifier.size(15.dp))
            Spacer(Modifier.width(5.dp))
            Text(
                label, color = foreground, fontSize = 11.sp, fontWeight = FontWeight.SemiBold,
                maxLines = 1, overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

@Composable
internal fun DeckLabel(text: String, modifier: Modifier = Modifier, color: Color = Secondary) {
    Text(
        text.uppercase(),
        color = color, fontSize = 11.sp, fontWeight = FontWeight.SemiBold,
        letterSpacing = 0.6.sp,
        modifier = modifier,
    )
}

/** A section heading with an optional trailing refresh. */
@Composable
internal fun DeckSectionHeader(
    title: String,
    hint: String? = null,
    loading: Boolean = false,
    onRefresh: (() -> Unit)? = null,
) {
    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            DeckLabel(title)
            hint?.let { Text(it, color = Muted, fontSize = 11.sp, lineHeight = 14.sp) }
        }
        if (loading) {
            CircularProgressIndicator(
                color = Coral, strokeWidth = 2.dp, modifier = Modifier.size(16.dp),
            )
        } else if (onRefresh != null) {
            Icon(
                Icons.Outlined.Refresh, "Refresh $title", tint = Coral,
                modifier = Modifier.size(32.dp).clickable(role = Role.Button, onClick = onRefresh).padding(7.dp),
            )
        }
    }
}

@Composable
internal fun ObserveBanner(message: String, tone: Color = Danger) {
    Surface(
        color = tone.copy(alpha = 0.11f),
        shape = RoundedCornerShape(10.dp),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Text(message, color = tone, fontSize = 12.sp, lineHeight = 16.sp, modifier = Modifier.padding(12.dp))
    }
}

/** Loading row used by every lane while it has nothing to show yet. */
@Composable
internal fun DeckLoadingRow(text: String) {
    Row(
        Modifier.fillMaxWidth().padding(vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(9.dp),
    ) {
        CircularProgressIndicator(color = Coral, strokeWidth = 2.dp, modifier = Modifier.size(15.dp))
        Text(text, color = Muted, fontSize = 12.sp)
    }
}

/** An error line with Retry, for lanes that report a plain sentence. */
@Composable
internal fun DeckErrorRow(message: String, onRetry: (() -> Unit)?) {
    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
        Text(message, color = Danger, fontSize = 12.sp, lineHeight = 16.sp, modifier = Modifier.weight(1f))
        if (onRetry != null) {
            TextButton(onClick = onRetry) { Text("Retry", color = Coral, fontSize = 12.sp) }
        }
    }
}

/** Small pill: LIVE, ON/OFF, badge text. */
@Composable
internal fun DeckPill(text: String, color: Color, modifier: Modifier = Modifier) {
    Surface(color = color.copy(alpha = 0.14f), shape = CircleShape, modifier = modifier) {
        Text(
            text, color = color, fontSize = 9.sp, fontWeight = FontWeight.Bold,
            maxLines = 1, softWrap = false, letterSpacing = 0.4.sp,
            modifier = Modifier.padding(horizontal = 6.dp, vertical = 2.dp),
        )
    }
}

/** A tinted rounded-square icon badge, the KPI and launchpad glyph. */
@Composable
internal fun DeckIconBadge(icon: ImageVector, tint: Color, size: Int = 24, iconSize: Int = 15) {
    Box(
        Modifier.size(size.dp).background(tint.copy(alpha = 0.14f), RoundedCornerShape(6.dp)),
        contentAlignment = Alignment.Center,
    ) {
        Icon(icon, null, tint = tint, modifier = Modifier.size(iconSize.dp))
    }
}

@Composable
internal fun DeckDivider() {
    Box(Modifier.fillMaxWidth().height(1.dp).background(BorderSoft))
}
