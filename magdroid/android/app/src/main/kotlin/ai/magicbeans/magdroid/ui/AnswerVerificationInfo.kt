package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.chat.AnswerVerificationNote
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Info
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

@Composable
internal fun AnswerVerificationInfo(note: AnswerVerificationNote) {
    var showing by remember { mutableStateOf(false) }
    Box {
        IconButton(onClick = { showing = true }, modifier = Modifier.size(44.dp)) {
            Icon(Icons.Outlined.Info, contentDescription = "Not fully verified — view details", tint = MWarn, modifier = Modifier.size(16.dp))
        }
        DropdownMenu(expanded = showing, onDismissRequest = { showing = false },
            containerColor = Control, modifier = Modifier.widthIn(max = 320.dp)) {
            Column(Modifier.padding(horizontal = 12.dp, vertical = 6.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text("Not fully verified", color = Ink, fontSize = 15.sp, fontWeight = FontWeight.SemiBold)
                SelectionContainer {
                    Text(note.details ?: AnswerVerificationNote.EXPLANATION, color = Ink, fontSize = 13.sp, lineHeight = 18.sp)
                }
            }
        }
    }
}
