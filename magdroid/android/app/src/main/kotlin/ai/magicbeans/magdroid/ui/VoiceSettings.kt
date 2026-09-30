package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.voice.SttSource
import ai.magicbeans.magdroid.voice.TtsEngine
import ai.magicbeans.magdroid.voice.LiveVoiceEngine
import ai.magicbeans.magdroid.voice.RealtimeVoiceProfile
import ai.magicbeans.magdroid.voice.RealtimeVoiceState
import ai.magicbeans.magdroid.voice.AudioStageOption
import ai.magicbeans.magdroid.voice.AudioSurfaceProfile
import ai.magicbeans.magdroid.voice.NativeAudioStage
import ai.magicbeans.magdroid.voice.NativeAudioSurface
import ai.magicbeans.magdroid.voice.audioProfileLabel
import ai.magicbeans.magdroid.voice.supports
import ai.magicbeans.magdroid.voice.usable
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.VolumeUp
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.outlined.GraphicEq
import androidx.compose.material.icons.outlined.Inventory2
import androidx.compose.material.icons.outlined.Mic
import androidx.compose.material.icons.outlined.Sensors
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material.icons.outlined.Tune
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * The three things "voice" means, kept apart.
 *
 * iOS splits this sheet into Replies, Dictation and Live call, and opens it on
 * whichever one the chevron you pressed belongs to. That matters more than it
 * looks: the settings are unrelated to each other, and one flat list of
 * everything makes the owner read all of it to change any of it.
 */
enum class VoiceSection(val title: String, val icon: ImageVector) {
    Replies("Replies", Icons.AutoMirrored.Outlined.VolumeUp),
    Dictation("Dictation", Icons.Outlined.Mic),
    Live("Live call", Icons.Outlined.Sensors),
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun VoiceSettingsSheet(
    initialSection: VoiceSection,
    speakReplies: Boolean,
    listening: Boolean,
    sttSource: SttSource,
    ttsEngine: TtsEngine,
    archiveDictation: Boolean,
    realtimeState: RealtimeVoiceState,
    handsFreeAvailable: Boolean,
    liveEngine: LiveVoiceEngine,
    realtimeProfiles: List<RealtimeVoiceProfile>,
    selectedRealtimeProfile: String?,
    livePushToTalk: Boolean,
    audioProfileCatalog: Map<String, AudioSurfaceProfile>,
    audioStageCatalog: Map<String, List<AudioStageOption>>,
    selectedAudioProfiles: Map<NativeAudioSurface, String>,
    selectedAudioStageOptions: Map<Pair<NativeAudioSurface, NativeAudioStage>, String>,
    onToggleSpeak: () -> Unit,
    onToggleWake: () -> Unit,
    onPickStt: (SttSource) -> Unit,
    onPickTts: (TtsEngine) -> Unit,
    onToggleArchive: () -> Unit,
    onPickLiveEngine: (LiveVoiceEngine) -> Unit,
    onPickRealtimeProfile: (String) -> Unit,
    onPickLivePushToTalk: (Boolean) -> Unit,
    onPickAudioProfile: (NativeAudioSurface, String) -> Unit,
    onPickAudioStageOption: (NativeAudioSurface, NativeAudioStage, String?) -> Unit,
    onRefreshVoiceCatalog: () -> Unit,
    onToggleLive: () -> Unit,
    onDismiss: () -> Unit,
) {
    var section by remember { mutableStateOf(initialSection) }
    // The session is registered with one immutable engine/profile snapshot.
    // Keep showing that snapshot until Stop completes even if another surface
    // changes the saved preference in the meantime.
    val effectiveLiveEngine = if (realtimeState.active) realtimeState.engine else liveEngine
    val handsFreeProfiles = audioProfileCatalog.values.filter {
        it.surface == NativeAudioSurface.HandsFree.wire
    }
    val usableHandsFree = handsFreeAvailable && (
        handsFreeProfiles.isEmpty() || handsFreeProfiles.any {
            it.usable(NativeAudioSurface.HandsFree, audioStageCatalog)
        }
    )
    ModalBottomSheet(
        onDismissRequest = onDismiss,
        containerColor = Ground,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
    ) {
        Column(Modifier.padding(bottom = 24.dp)) {
            Row(
                Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text("Voice settings", color = Ink, fontSize = 17.sp, fontWeight = FontWeight.SemiBold)
                Spacer(Modifier.weight(1f))
                Text(
                    "Done",
                    color = Coral, fontSize = 15.sp, fontWeight = FontWeight.SemiBold,
                    modifier = Modifier.clickable { onDismiss() }.padding(6.dp),
                )
            }
            SectionPicker(section) { section = it }
            Column(
                Modifier
                    .heightIn(max = 460.dp)
                    .verticalScroll(rememberScrollState())
                    .padding(horizontal = 16.dp, vertical = 10.dp),
                verticalArrangement = Arrangement.spacedBy(20.dp),
            ) {
                when (section) {
                    VoiceSection.Replies -> {
                        SettingsGroup {
                            ToggleRow(
                                title = "Speak replies",
                                subtitle = if (realtimeState.active) {
                                    "Temporarily locked during the active call"
                                } else {
                                    "Read assistant replies aloud"
                                },
                                icon = Icons.AutoMirrored.Outlined.VolumeUp,
                                checked = speakReplies,
                                onToggle = onToggleSpeak,
                                enabled = !realtimeState.active,
                            )
                        }
                        SectionHeader("Reply voice", Icons.Outlined.GraphicEq)
                        SettingsGroup {
                            TtsEngine.entries.forEachIndexed { index, engine ->
                                SelectionRow(
                                    title = engine.label,
                                    selected = ttsEngine == engine,
                                    enabled = true,
                                ) { onPickTts(engine) }
                                if (index < TtsEngine.entries.lastIndex) SettingsDivider()
                            }
                        }
                    }

                    VoiceSection.Dictation -> {
                        SectionHeader("Wake word", Icons.Outlined.Sensors)
                        SettingsGroup {
                            ToggleRow(
                                title = "Listen for “magician”",
                                subtitle = "Wake on your voice with the screen off. Audio stays on this device.",
                                icon = Icons.Outlined.Sensors,
                                checked = listening,
                                onToggle = onToggleWake,
                            )
                        }
                        Text(
                            "Android can keep listening in the background; iOS cannot. " +
                                "A notification shows the whole time, and stopping it is one tap from there.",
                            color = Muted, fontSize = 11.sp, lineHeight = 15.sp,
                        )

                        SectionHeader("Audio Notes", Icons.Outlined.Inventory2)
                        SettingsGroup {
                            ToggleRow(
                                title = "Keep dictation recordings",
                                subtitle = "Save the original audio and transcript as a durable Audio Note.",
                                icon = Icons.Outlined.Inventory2,
                                checked = archiveDictation,
                                onToggle = onToggleArchive,
                            )
                        }
                        Text(
                            "Off by default. When enabled, Android records one bounded WAV, stages it " +
                                "before upload, and retries from a durable outbox. This uses backend " +
                                "transcription because Android Speech does not expose its microphone bytes.",
                            color = Muted, fontSize = 11.sp, lineHeight = 15.sp,
                        )

                        SectionHeader("Transcription", Icons.Outlined.Mic)
                        SettingsGroup {
                            SttSource.entries.forEachIndexed { index, source ->
                                SelectionRow(
                                    title = source.label,
                                    selected = sttSource == source,
                                    enabled = true,
                                ) { onPickStt(source) }
                                if (index < SttSource.entries.lastIndex) SettingsDivider()
                            }
                        }
                        AudioPipelineSettings(
                            surface = NativeAudioSurface.Dictation,
                            profiles = audioProfileCatalog,
                            stages = audioStageCatalog,
                            selectedProfile = selectedAudioProfiles[NativeAudioSurface.Dictation],
                            selectedStages = selectedAudioStageOptions,
                            onPickProfile = onPickAudioProfile,
                            onPickStage = onPickAudioStageOption,
                            onRefresh = onRefreshVoiceCatalog,
                        )
                    }

                    VoiceSection.Live -> {
                        SectionHeader("Call mode", Icons.Outlined.GraphicEq)
                        ModePicker(
                            selected = effectiveLiveEngine,
                            realtimeAvailable = realtimeProfiles.any(RealtimeVoiceProfile::nativeAndAvailable),
                            handsFreeAvailable = usableHandsFree,
                            locked = realtimeState.active,
                            onSelect = onPickLiveEngine,
                        )
                        if (effectiveLiveEngine == LiveVoiceEngine.Realtime) {
                            SectionHeader("Realtime model", Icons.Outlined.Sensors)
                            val nativeProfiles = realtimeProfiles.filter(RealtimeVoiceProfile::native)
                            if (nativeProfiles.isEmpty()) {
                                SettingsGroup {
                                    KeyboardLikeActionRow("Refresh provider catalog", onRefreshVoiceCatalog)
                                }
                                realtimeProfiles.firstOrNull()?.unavailableReason?.let { EmptyGroupNote(it) }
                            } else {
                                SettingsGroup {
                                    nativeProfiles
                                        .forEachIndexed { index, profile ->
                                            SelectionRow(
                                                title = profile.label,
                                                subtitle = buildString {
                                                    append("${profile.provider} · ${profile.model}")
                                                    if (profile.mode == "translation") append(" · Translate")
                                                    if (!profile.available) {
                                                        append("\n")
                                                        append(profile.unavailableReason ?: "Unavailable")
                                                    }
                                                },
                                                selected = selectedRealtimeProfile == profile.id,
                                                enabled = !realtimeState.active && profile.available,
                                            ) { onPickRealtimeProfile(profile.id) }
                                            if (index < nativeProfiles.lastIndex) {
                                                SettingsDivider()
                                            }
                                        }
                                }
                            }
                        } else {
                            AudioPipelineSettings(
                                surface = NativeAudioSurface.HandsFree,
                                profiles = audioProfileCatalog,
                                stages = audioStageCatalog,
                                selectedProfile = selectedAudioProfiles[NativeAudioSurface.HandsFree],
                                selectedStages = selectedAudioStageOptions,
                                onPickProfile = onPickAudioProfile,
                                onPickStage = onPickAudioStageOption,
                                onRefresh = onRefreshVoiceCatalog,
                                enabled = !realtimeState.active,
                            )
                        }
                        val selectedProfile = realtimeProfiles.firstOrNull { it.id == selectedRealtimeProfile }
                        val pushToTalkAvailable = effectiveLiveEngine != LiveVoiceEngine.Realtime ||
                            selectedProfile?.mode != "translation"
                        SectionHeader("Microphone control", Icons.Outlined.Mic)
                        TurnBoundaryPicker(
                            pushToTalk = livePushToTalk && pushToTalkAvailable,
                            enabled = !realtimeState.active,
                            pushToTalkAvailable = pushToTalkAvailable,
                            onSelect = onPickLivePushToTalk,
                        )
                        Text(
                            when {
                                !pushToTalkAvailable -> "Translation uses open mic for continuous two-way audio."
                                livePushToTalk -> "The microphone sends audio only while you hold the talk control."
                                else -> "The microphone stays open; mute remains available during the call."
                            },
                            color = Muted, fontSize = 11.sp, lineHeight = 15.sp,
                        )
                        SectionHeader("Call status", Icons.Outlined.Sensors)
                        SettingsGroup {
                            Text(
                                when (realtimeState.phase) {
                                    RealtimeVoiceState.Phase.Idle -> "Ready"
                                    RealtimeVoiceState.Phase.Connecting -> "Connecting"
                                    RealtimeVoiceState.Phase.Reconnecting -> "Reconnecting"
                                    RealtimeVoiceState.Phase.Ready -> "Live"
                                    RealtimeVoiceState.Phase.Ending -> "Ending"
                                    RealtimeVoiceState.Phase.Failed -> "Call failed"
                                },
                                color = Ink,
                                fontSize = 14.sp,
                                modifier = Modifier.fillMaxWidth().padding(14.dp),
                            )
                        }
                        realtimeState.error?.let { EmptyGroupNote(it) }
                        Text(
                            if (effectiveLiveEngine == LiveVoiceEngine.Realtime) {
                                "Live streams bounded 24 kHz PCM through Magician's backend-proxied realtime " +
                                    "session. ${if (livePushToTalk && pushToTalkAvailable) "Hold to talk owns" else "Server VAD owns"} " +
                                    "turn boundaries and Stop closes both audio directions."
                            } else {
                                "Hands-free keeps the configured transcription → agent → reply voice " +
                                    "pipeline open between turns. Silence ends the call; Stop ends it immediately."
                            },
                            color = Muted, fontSize = 11.sp, lineHeight = 15.sp,
                        )
                    }
                }
            }
            if (section == VoiceSection.Live) {
                val active = realtimeState.active
                val available = when (effectiveLiveEngine) {
                    LiveVoiceEngine.HandsFree -> usableHandsFree
                    LiveVoiceEngine.Realtime -> realtimeProfiles.any(RealtimeVoiceProfile::nativeAndAvailable)
                }
                CallAction(active, available, effectiveLiveEngine) {
                    onToggleLive()
                    // Match iOS: after Start the call itself becomes the
                    // surface. Stop remains in the sheet so its terminal state
                    // and any error stay visible until the owner dismisses it.
                    if (!active) onDismiss()
                }
            }
        }
    }
}

@Composable
private fun TurnBoundaryPicker(
    pushToTalk: Boolean,
    enabled: Boolean,
    pushToTalkAvailable: Boolean,
    onSelect: (Boolean) -> Unit,
) {
    Surface(
        color = Panel,
        shape = RoundedCornerShape(8.dp),
        border = BorderStroke(1.dp, BorderSoft),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Row(Modifier.padding(3.dp), horizontalArrangement = Arrangement.spacedBy(4.dp)) {
            listOf(false to "Open mic", true to "Hold to talk").forEach { (ptt, title) ->
                val selected = pushToTalk == ptt
                val choiceEnabled = enabled && (!ptt || pushToTalkAvailable)
                Row(
                    Modifier.weight(1f).height(40.dp)
                        .alpha(if (choiceEnabled) 1f else 0.45f)
                        .background(if (selected) Coral else Color.Transparent, RoundedCornerShape(7.dp))
                        .clickable(enabled = choiceEnabled) { onSelect(ptt) },
                    horizontalArrangement = Arrangement.spacedBy(6.dp, Alignment.CenterHorizontally),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Icon(
                        if (ptt) Icons.Outlined.GraphicEq else Icons.Outlined.Mic,
                        contentDescription = null,
                        tint = if (selected) Color.White else Secondary,
                        modifier = Modifier.size(14.dp),
                    )
                    Text(
                        title,
                        color = if (selected) Color.White else Secondary,
                        fontSize = 12.sp,
                        fontWeight = FontWeight.SemiBold,
                    )
                }
            }
        }
    }
}

@Composable
private fun AudioPipelineSettings(
    surface: NativeAudioSurface,
    profiles: Map<String, AudioSurfaceProfile>,
    stages: Map<String, List<AudioStageOption>>,
    selectedProfile: String?,
    selectedStages: Map<Pair<NativeAudioSurface, NativeAudioStage>, String>,
    onPickProfile: (NativeAudioSurface, String) -> Unit,
    onPickStage: (NativeAudioSurface, NativeAudioStage, String?) -> Unit,
    onRefresh: () -> Unit,
    enabled: Boolean = true,
) {
    val candidates = profiles.entries.filter { it.value.surface == surface.wire }
    SectionHeader("${surface.label} pipeline", Icons.Outlined.GraphicEq)
    if (candidates.isEmpty()) {
        SettingsGroup { KeyboardLikeActionRow("Refresh audio profiles", onRefresh, enabled) }
        return
    }
    val activeEntry = candidates.firstOrNull { it.key == selectedProfile } ?: candidates.first()
    SettingsGroup {
        candidates.forEachIndexed { index, (id, profile) ->
            SelectionRow(
                title = audioProfileLabel(id),
                selected = id == activeEntry.key,
                enabled = enabled && profile.usable(surface, stages),
            ) {
                NativeAudioStage.entries.forEach { stage ->
                    val selected = selectedStages[surface to stage] ?: return@forEach
                    val option = stages[stage.wire].orEmpty().firstOrNull { it.id == selected }
                    if (option == null || !profile.supports(option, stage)) {
                        onPickStage(surface, stage, null)
                    }
                }
                onPickProfile(surface, id)
            }
            if (index < candidates.lastIndex) SettingsDivider()
        }
    }
    NativeAudioStage.entries.filter { activeEntry.value.stage(it).enabled }.forEach { stage ->
        SectionHeader(stage.label, Icons.Outlined.Tune)
        val options = stages[stage.wire].orEmpty().filter { activeEntry.value.supports(it, stage) }
        SettingsGroup {
            val selected = selectedStages[surface to stage]
            SelectionRow(
                title = "Profile order",
                selected = selected == null,
                enabled = enabled,
            ) { onPickStage(surface, stage, null) }
            options.forEach { option ->
                SettingsDivider()
                SelectionRow(
                    title = option.displayLabel,
                    selected = selected == option.id,
                    enabled = enabled && option.available,
                ) { onPickStage(surface, stage, option.id) }
            }
        }
    }
}

/**
 * Start the call, pinned below the settings that configure it.
 *
 * Outside the scroll on purpose: it is the reason the section exists, and
 * having to scroll to reach the button that acts on what you just changed is
 * how a settings screen becomes a puzzle. Disabled here, and dimmed to say so,
 * because there is no call to start.
 */
@Composable
private fun CallAction(
    active: Boolean,
    available: Boolean,
    engine: LiveVoiceEngine,
    onToggle: () -> Unit,
) {
    Column {
        HorizontalDivider(color = BorderSoft)
        Surface(
            color = if (active) Danger else Coral,
            shape = RoundedCornerShape(8.dp),
            modifier = Modifier
                .fillMaxWidth()
                .padding(start = 16.dp, end = 16.dp, top = 10.dp, bottom = 12.dp)
                .height(46.dp)
                .alpha(if (available || active) 1f else 0.45f)
                .clickable(enabled = available || active) { onToggle() },
        ) {
            Row(
                Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.CenterHorizontally),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Icon(
                    Icons.Outlined.Sensors, contentDescription = null,
                    tint = Color.White, modifier = Modifier.size(17.dp),
                )
                Text(
                    when {
                        active -> "Stop call"
                        !available -> "Selected setup unavailable"
                        engine == LiveVoiceEngine.Realtime -> "Start live call"
                        else -> "Start hands-free"
                    },
                    color = Color.White, fontSize = 15.sp, fontWeight = FontWeight.SemiBold,
                )
            }
        }
    }
}

/**
 * Android's working open-conversation engine. Vendor low-latency realtime is
 * not rendered as a selectable mode until the native PCM transport exists;
 * presenting an inert alternative was the settings regression this replaces.
 */
@Composable
private fun ModePicker(
    selected: LiveVoiceEngine,
    realtimeAvailable: Boolean,
    handsFreeAvailable: Boolean,
    locked: Boolean,
    onSelect: (LiveVoiceEngine) -> Unit,
) {
    Surface(
        color = Panel,
        shape = RoundedCornerShape(8.dp),
        border = BorderStroke(1.dp, BorderSoft),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Row(Modifier.padding(3.dp), horizontalArrangement = Arrangement.spacedBy(4.dp)) {
            LiveVoiceEngine.entries.forEach { engine ->
                val active = selected == engine
                val enabled = !locked && if (engine == LiveVoiceEngine.HandsFree) handsFreeAvailable else realtimeAvailable
                Row(
                    Modifier
                        .weight(1f)
                        .height(40.dp)
                        .alpha(if (enabled) 1f else 0.45f)
                        .background(if (active) Coral else Color.Transparent, RoundedCornerShape(7.dp))
                        .clickable(enabled = enabled) { onSelect(engine) },
                    horizontalArrangement = Arrangement.spacedBy(6.dp, Alignment.CenterHorizontally),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Icon(
                        if (engine == LiveVoiceEngine.Realtime) Icons.Outlined.Sensors else Icons.Outlined.GraphicEq,
                        contentDescription = null,
                        tint = if (active) Color.White else Secondary,
                        modifier = Modifier.size(14.dp),
                    )
                    Text(
                        engine.label,
                        color = if (active) Color.White else Secondary,
                        fontSize = 12.sp,
                        fontWeight = FontWeight.SemiBold,
                    )
                }
            }
        }
    }
}

@Composable
private fun KeyboardLikeActionRow(title: String, onClick: () -> Unit, enabled: Boolean = true) {
    Row(
        Modifier.fillMaxWidth().alpha(if (enabled) 1f else 0.45f)
            .clickable(enabled = enabled) { onClick() }.padding(horizontal = 12.dp, vertical = 12.dp),
        horizontalArrangement = Arrangement.spacedBy(10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(Icons.Outlined.Refresh, contentDescription = null, tint = Coral, modifier = Modifier.size(17.dp))
        Text(title, color = Ink, fontSize = 14.sp)
    }
}

/** A group whose list is empty, saying so where the list would have been. */
@Composable
private fun EmptyGroupNote(text: String) {
    Surface(color = Panel, shape = RoundedCornerShape(8.dp), border = BorderStroke(1.dp, BorderSoft)) {
        Text(
            text,
            color = Muted, fontSize = 13.sp,
            modifier = Modifier.fillMaxWidth().padding(14.dp),
        )
    }
}

/**
 * The section selector: one segment each, the chosen one filled.
 *
 * A segmented control rather than tabs because there are three fixed choices
 * and all of them fit — the shape says "these are alternatives" without
 * anything having to scroll.
 */
@Composable
private fun SectionPicker(selected: VoiceSection, onSelect: (VoiceSection) -> Unit) {
    Surface(
        color = Panel,
        shape = RoundedCornerShape(8.dp),
        border = BorderStroke(1.dp, BorderSoft),
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
    ) {
        Row(
            Modifier.padding(3.dp),
            horizontalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            VoiceSection.entries.forEach { entry ->
                val active = entry == selected
                Surface(
                    color = if (active) Coral else Color.Transparent,
                    shape = RoundedCornerShape(7.dp),
                    modifier = Modifier
                        .weight(1f)
                        .height(38.dp)
                        .clickable { onSelect(entry) },
                ) {
                    Row(
                        Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.spacedBy(4.dp, Alignment.CenterHorizontally),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Icon(
                            entry.icon,
                            contentDescription = null,
                            tint = if (active) Color.White else Secondary,
                            modifier = Modifier.size(14.dp),
                        )
                        Text(
                            entry.title,
                            color = if (active) Color.White else Secondary,
                            fontSize = 12.sp,
                            fontWeight = FontWeight.SemiBold,
                        )
                    }
                }
            }
        }
    }
}

/** Related settings share one bordered card, as they do on iOS. */
@Composable
private fun SettingsGroup(content: @Composable () -> Unit) {
    Surface(color = Panel, shape = RoundedCornerShape(8.dp), border = BorderStroke(1.dp, BorderSoft)) {
        Column { content() }
    }
}

@Composable
private fun SettingsDivider() {
    HorizontalDivider(color = BorderSoft, modifier = Modifier.padding(start = 12.dp))
}

@Composable
private fun SectionHeader(title: String, icon: ImageVector) {
    Row(
        horizontalArrangement = Arrangement.spacedBy(6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(icon, contentDescription = null, tint = Secondary, modifier = Modifier.size(13.dp))
        Text(title, color = Secondary, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
    }
}

@Composable
private fun ToggleRow(
    title: String,
    subtitle: String,
    icon: ImageVector,
    checked: Boolean,
    onToggle: () -> Unit,
    enabled: Boolean = true,
) {
    Row(
        Modifier
            .fillMaxWidth()
            .alpha(if (enabled) 1f else 0.6f)
            .clickable(enabled = enabled) { onToggle() }
            .padding(horizontal = 12.dp, vertical = 11.dp),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(Modifier.width(22.dp), contentAlignment = Alignment.Center) {
            Icon(icon, contentDescription = null, tint = Coral, modifier = Modifier.size(15.dp))
        }
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(title, color = Ink, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
            Text(subtitle, color = Muted, fontSize = 11.sp, lineHeight = 15.sp, maxLines = 2)
        }
        Switch(
            checked = checked,
            enabled = enabled,
            onCheckedChange = { onToggle() },
            colors = SwitchDefaults.colors(
                checkedThumbColor = Color.White,
                checkedTrackColor = Coral,
            ),
        )
    }
}

@Composable
private fun SelectionRow(
    title: String,
    subtitle: String? = null,
    selected: Boolean,
    enabled: Boolean,
    onSelect: () -> Unit,
) {
    Row(
        Modifier
            .fillMaxWidth()
            .background(if (selected) Coral.copy(alpha = 0.08f) else Color.Transparent)
            .alpha(if (enabled) 1f else 0.45f)
            .clickable(enabled = enabled) { onSelect() }
            .padding(horizontal = 12.dp, vertical = 11.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(
                title,
                color = Ink,
                fontSize = 13.sp,
                fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Normal,
            )
            subtitle?.takeIf(String::isNotBlank)?.let {
                Text(it, color = Muted, fontSize = 10.sp, lineHeight = 13.sp)
            }
        }
        if (selected) {
            Icon(
                Icons.Filled.CheckCircle,
                contentDescription = "Selected",
                tint = Coral,
                modifier = Modifier.size(17.dp),
            )
        }
    }
}
