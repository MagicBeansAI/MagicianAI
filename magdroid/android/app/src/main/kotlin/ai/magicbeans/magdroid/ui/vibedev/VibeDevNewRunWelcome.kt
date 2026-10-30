package ai.magicbeans.magdroid.ui.vibedev

import ai.magicbeans.magdroid.ui.LocalMagicanFontFamilies
import ai.magicbeans.magdroid.ui.activePalette
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.AutoAwesome
import androidx.compose.material.icons.outlined.BugReport
import androidx.compose.material.icons.outlined.TravelExplore
import androidx.compose.material3.Icon
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/** A quiet welcome above the composer; starters fill a draft and never send it. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun VibeDevNewRunWelcome(title: String, onStart: (String, String) -> Unit) {
    val palette = activePalette
    val fonts = LocalMagicanFontFamilies.current
    val typing = WindowInsets.ime.getBottom(LocalDensity.current) > 0
    Column(Modifier.fillMaxWidth().padding(vertical = 20.dp), horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(16.dp)) {
        if (!typing) {
            Box(Modifier.size(112.dp).background(Brush.radialGradient(listOf(palette.accentFill.copy(alpha = 0.12f), palette.accentFill.copy(alpha = 0f)))),
                contentAlignment = Alignment.Center) {
                Surface(color = palette.accentFill.copy(alpha = 0.09f), shape = RoundedCornerShape(14.dp),
                    modifier = Modifier.size(76.dp, 66.dp).rotate(-12f),
                    border = BorderStroke(1.dp, palette.accentFill.copy(alpha = 0.12f))) {}
                Surface(color = palette.surface, shape = RoundedCornerShape(14.dp), shadowElevation = 3.dp,
                    border = BorderStroke(1.dp, palette.accentFill.copy(alpha = 0.25f)), modifier = Modifier.size(76.dp, 66.dp)) {
                    Box(contentAlignment = Alignment.Center) {
                        Text("</>", color = palette.accentFill, fontFamily = fonts.mono, fontSize = 27.sp, fontWeight = FontWeight.SemiBold)
                    }
                }
                Surface(color = palette.elevated, shape = RoundedCornerShape(7.dp),
                    modifier = Modifier.align(Alignment.TopEnd).padding(top = 9.dp, end = 7.dp),
                    border = BorderStroke(1.dp, palette.accentFill.copy(alpha = 0.2f))) {
                    Icon(Icons.Outlined.AutoAwesome, null, tint = palette.accentFill, modifier = Modifier.padding(6.dp).size(17.dp))
                }
            }
        }
        Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(10.dp)) {
            Text(title.uppercase(), color = palette.accentFill, fontFamily = fonts.mono,
                fontSize = 9.sp, fontWeight = FontWeight.SemiBold, letterSpacing = 1.2.sp, textAlign = TextAlign.Center)
            Text("What will you build?", color = palette.text, fontFamily = fonts.display,
                fontSize = if (typing) 23.sp else 28.sp, lineHeight = 34.sp, fontWeight = FontWeight.SemiBold, textAlign = TextAlign.Center)
            Text("Build a feature, fix a bug, or get to know your code. Describe your next step below.",
                modifier = Modifier.widthIn(max = 300.dp), color = palette.secondaryText, fontFamily = fonts.body,
                fontSize = 14.sp, lineHeight = 21.sp, textAlign = TextAlign.Center)
        }
        if (!typing) {
            FlowRow(Modifier.fillMaxWidth().padding(top = 2.dp), horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.CenterHorizontally),
                verticalArrangement = Arrangement.spacedBy(8.dp)) {
                WelcomeStarter("Build a feature", Icons.Outlined.Add) { onStart("build", "Build a feature in this project: ") }
                WelcomeStarter("Fix a bug", Icons.Outlined.BugReport) { onStart("build", "Help me diagnose and fix this issue: ") }
                WelcomeStarter("Explore the code", Icons.Outlined.TravelExplore) { onStart("discuss", "Explain what this project does and how its main parts fit together.") }
            }
        }
    }
}

@Composable
private fun WelcomeStarter(title: String, icon: ImageVector, onClick: () -> Unit) {
    val palette = activePalette
    Surface(color = palette.accentFill.copy(alpha = 0.045f), shape = RoundedCornerShape(8.dp),
        border = BorderStroke(1.dp, palette.secondaryText.copy(alpha = 0.16f)),
        modifier = Modifier.clickable(role = Role.Button, onClick = onClick)) {
        Row(Modifier.padding(horizontal = 11.dp, vertical = 10.dp), verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            Icon(icon, null, tint = palette.accentFill, modifier = Modifier.size(14.dp))
            Text(title, color = palette.text, fontFamily = LocalMagicanFontFamilies.current.body, fontSize = 12.sp, fontWeight = FontWeight.Medium)
        }
    }
}
