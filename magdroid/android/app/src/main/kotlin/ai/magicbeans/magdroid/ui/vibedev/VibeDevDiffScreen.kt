package ai.magicbeans.magdroid.ui.vibedev

import ai.magicbeans.magdroid.ui.LocalMagicanFontFamilies
import ai.magicbeans.magdroid.ui.activePalette
import ai.magicbeans.magdroid.vibedev.VibeDevDiffViewModel
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle

/** One file's diff, with the file's place among the run's changes and previous/next. */
@Composable
internal fun VibeDevDiffScreen(chrome: VibeDevChrome, model: VibeDevDiffViewModel) {
    AttachWhileVisible(model, chrome)
    val data by model.data.collectAsStateWithLifecycle()
    val status by model.loadStatus.collectAsStateWithLifecycle()
    val file = data.file
    val diff = data.diff

    Column(Modifier.fillMaxSize().background(activePalette.background)) {
        Column(Modifier.padding(horizontal = 16.dp, vertical = 8.dp)) {
            file?.let {
                Text(it.path, color = activePalette.text, fontSize = 12.5.sp, fontFamily = LocalMagicanFontFamilies.current.mono)
                Row(horizontalArrangement = Arrangement.spacedBy(10.dp), verticalAlignment = Alignment.CenterVertically) {
                    DiffCounts(it.additions, it.deletions)
                    MetaText("File ${data.index + 1} of ${data.files.size}", maxLines = 1)
                }
            }
            if (data.patch?.truncated == true) {
                MetaText("This file is too large to show whole here. Open the run on the desktop for the rest.", maxLines = 3)
            }
            StaleLine(status, diff != null, onRetry = { model.refresh() })
        }
        HorizontalDivider(color = activePalette.cardBorder)
        when {
            diff == null && status.error == null -> Column(Modifier.weight(1f)) { LoadingLine() }
            diff == null -> Column(Modifier.weight(1f)) {}
            diff.isEmpty -> Column(Modifier.weight(1f).padding(16.dp)) {
                EmptyLine(if (file?.status == "D") "This file was deleted." else "No text changes to show (binary or renamed).")
            }
            else -> UnifiedDiffView(
                diff = diff,
                modifier = Modifier.weight(1f).fillMaxWidth(),
                wrap = chrome.wrap,
                collapsed = data.collapsed,
                unfolded = data.unfolded,
                onToggleHunk = model::toggleHunk,
                onUnfold = model::unfold,
            )
        }
        Surface(color = activePalette.surface, modifier = Modifier.fillMaxWidth()) {
            Column {
                HorizontalDivider(color = activePalette.cardBorder)
                Row(
                    Modifier.fillMaxWidth().navigationBarsPadding().padding(horizontal = 16.dp, vertical = 10.dp),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    VibeButton("‹ Previous file", { model.step(-1) }, Modifier.weight(1f), enabled = data.hasPrevious)
                    VibeButton("Next file ›", { model.step(1) }, Modifier.weight(1f), enabled = data.hasNext)
                }
            }
        }
    }
}
