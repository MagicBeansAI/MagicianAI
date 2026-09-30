package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.today.OPS_CAROUSEL_ADVANCE_MS
import ai.magicbeans.magdroid.today.OPS_CAROUSEL_MANUAL_PAUSE_MS
import ai.magicbeans.magdroid.today.TodayCrew
import ai.magicbeans.magdroid.today.TodayCrewMember
import ai.magicbeans.magdroid.today.TodayTaskLine
import ai.magicbeans.magdroid.today.avgCostPerCall
import ai.magicbeans.magdroid.today.formatPerCall
import ai.magicbeans.magdroid.today.peakSpendHour
import ai.magicbeans.magdroid.today.carouselMayAdvance
import ai.magicbeans.magdroid.today.nextCarouselIndex
import ai.magicbeans.magdroid.today.taskShortAge
import android.provider.Settings
import androidx.compose.animation.core.animateDpAsState
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.interaction.collectIsDraggedAsState
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.selected
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.SizeTransform
import androidx.compose.animation.togetherWith
import androidx.compose.animation.animateContentSize
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.tween
import androidx.compose.animation.expandVertically
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.shrinkVertically
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.KeyboardArrowDown
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.semantics.stateDescription
import ai.magicbeans.magdroid.today.SpendTone
import ai.magicbeans.magdroid.today.TodayAgentCounts
import ai.magicbeans.magdroid.today.TodayFleetYield
import ai.magicbeans.magdroid.today.TodayPulse
import ai.magicbeans.magdroid.today.hourLabel
import ai.magicbeans.magdroid.today.hourlySpendTooltip
import ai.magicbeans.magdroid.today.spendDeltaLine
import ai.magicbeans.magdroid.today.spendTone
import ai.magicbeans.magdroid.today.splitSpend
import ai.magicbeans.magdroid.today.todayFleetYield
import ai.magicbeans.magdroid.today.topProviderShareLabel
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectHorizontalDragGestures
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import java.time.LocalTime
import java.util.Locale

/** A carousel slide: its header title and its content for the shared collapsed/expanded state. */
private class OpsSlide(val title: String, val content: @Composable (expanded: Boolean) -> Unit)

/** Fixed content heights so neither slide changes nor auto-advance makes the page jump. */
private val OPS_COLLAPSED_HEIGHT = 62.dp
private val OPS_EXPANDED_HEIGHT = 206.dp
/** One task row; the list shows three and a half so the cut-off row says it scrolls. */
private val OPS_TASK_ROW = 27.dp

/**
 * The operations carousel: Economics of Operations, then State of
 * Operations; more slides are one entry in [slides]. It auto-advances every 8 s
 * (held 15 s after a swipe or dot tap, never with animations off), swipes by
 * hand, and shows dots. One chevron collapses or expands every slide.
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
internal fun TodayLedger(
    pulse: TodayPulse?,
    agents: TodayAgentCounts?,
    pulseError: String?,
    onRetry: () -> Unit,
    onOpenTasks: () -> Unit,
    onOpenTask: (String) -> Unit,
    crew: TodayCrew? = null,
    crewError: String? = null,
    onRetryCrew: () -> Unit = {},
    modifier: Modifier = Modifier,
) {
    var expanded by rememberSaveable { mutableStateOf(false) }
    val chevron by animateFloatAsState(if (expanded) 180f else 0f, tween(260), label = "opsChevron")
    val current = pulse ?: TodayPulse()
    val fleet = todayFleetYield(current.taskBuckets, current.tasksCompletedToday)
    val slides = listOf(
        OpsSlide("Economics of Operations") { full -> SpendPanel(current, full) },
        OpsSlide("State of Operations") { full -> StateOfOperations(fleet, agents, current.recentTasks, full, onOpenTasks, onOpenTask) },
        OpsSlide("State of the Crew") { full -> StateOfTheCrew(crew, crewError, full, onRetryCrew) },
    )
    val pager = rememberPagerState { slides.size }
    val scope = rememberCoroutineScope()
    val context = LocalContext.current
    val reduceMotion = remember {
        runCatching { Settings.Global.getFloat(context.contentResolver, Settings.Global.ANIMATOR_DURATION_SCALE, 1f) == 0f }.getOrDefault(false)
    }
    val dragging by pager.interactionSource.collectIsDraggedAsState()
    var pausedUntil by remember { mutableLongStateOf(0L) }
    LaunchedEffect(dragging) { if (dragging) pausedUntil = System.currentTimeMillis() + OPS_CAROUSEL_MANUAL_PAUSE_MS }
    LaunchedEffect(slides.size, reduceMotion) {
        while (true) {
            delay(OPS_CAROUSEL_ADVANCE_MS)
            if (carouselMayAdvance(System.currentTimeMillis(), pausedUntil, pager.isScrollInProgress, reduceMotion)) {
                pager.animateScrollToPage(nextCarouselIndex(pager.currentPage, slides.size), animationSpec = tween(520))
            }
        }
    }
    val height by animateDpAsState(if (expanded) OPS_EXPANDED_HEIGHT else OPS_COLLAPSED_HEIGHT, tween(280), label = "opsHeight")
    Column(
        modifier.fillMaxWidth()
            .background(Panel, RoundedCornerShape(4.dp))
            .border(1.dp, BorderSoft, RoundedCornerShape(4.dp)),
    ) {
        Box(Modifier.fillMaxWidth().height(2.dp).background(Ink))
        Column(Modifier.padding(horizontal = 14.dp, vertical = 11.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
            Row(
                Modifier.fillMaxWidth()
                    .clickable(onClickLabel = if (expanded) "Collapse" else "Expand") { expanded = !expanded }
                    .semantics { stateDescription = if (expanded) "Expanded" else "Collapsed" },
                verticalAlignment = Alignment.CenterVertically,
            ) {
                AnimatedContent(
                    targetState = pager.currentPage,
                    transitionSpec = { fadeIn(tween(220)) togetherWith fadeOut(tween(160)) },
                    modifier = Modifier.weight(1f),
                    label = "opsTitle",
                ) { page ->
                    FitText(slides[page].title, style = newsSerif(20.sp, FontWeight.ExtraBold), modifier = Modifier.semantics { heading() })
                }
                Icon(Icons.Outlined.KeyboardArrowDown, null, tint = Coral, modifier = Modifier.size(22.dp).rotate(chevron))
            }
            Hairline()
            pulseError?.takeIf { pulse == null }?.let { TodayInlineError(it, onRetry) }
            HorizontalPager(
                state = pager,
                modifier = Modifier.fillMaxWidth().height(height)
                    .semantics { contentDescription = "Operations, slide ${pager.currentPage + 1} of ${slides.size}, ${slides[pager.currentPage].title}" },
                verticalAlignment = Alignment.Top,
                pageSpacing = 16.dp,
            ) { page ->
                Box(Modifier.fillMaxSize()) { slides[page].content(expanded) }
            }
            OpsDots(count = slides.size, current = pager.currentPage, titles = slides.map(OpsSlide::title)) { target ->
                pausedUntil = System.currentTimeMillis() + OPS_CAROUSEL_MANUAL_PAUSE_MS
                scope.launch { if (reduceMotion) pager.scrollToPage(target) else pager.animateScrollToPage(target) }
            }
        }
        Box(Modifier.fillMaxWidth().height(2.dp).background(Ink))
    }
}

/** Slide position dots: the current one is an accent pill, the rest muted circles. */
@Composable
private fun OpsDots(count: Int, current: Int, titles: List<String>, onSelect: (Int) -> Unit) {
    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.Center, verticalAlignment = Alignment.CenterVertically) {
        repeat(count) { index ->
            val selected = index == current
            val width by animateDpAsState(if (selected) 16.dp else 6.dp, tween(220), label = "opsDot")
            Box(
                Modifier
                    .clickable(onClickLabel = "Show ${titles[index]}") { onSelect(index) }
                    .semantics { contentDescription = "Slide ${index + 1} of $count, ${titles[index]}"; this.selected = selected }
                    .padding(horizontal = 4.dp, vertical = 6.dp)
                    .size(width = width, height = 6.dp)
                    .background(if (selected) Coral else BorderSoft, RoundedCornerShape(3.dp)),
            )
        }
    }
}

/**
 * Slide 2. Collapsed: Active / Succeeded / Failed counts and a mini pie.
 * Expanded: pie with legend, agent counts, and the most recently updated tasks
 * one line each, scrolling inside the slide.
 */
@Composable
private fun StateOfOperations(
    fleet: TodayFleetYield,
    agents: TodayAgentCounts?,
    tasks: List<TodayTaskLine>,
    expanded: Boolean,
    onOpenTasks: () -> Unit,
    onOpenTask: (String) -> Unit,
) {
    AnimatedContent(
        targetState = expanded,
        transitionSpec = { fadeIn(tween(220, delayMillis = 60)) togetherWith fadeOut(tween(140)) },
        label = "stateOfOps",
    ) { full ->
        if (!full) {
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                OpsStat("Active", fleet.inFlight, TodayWarning, Modifier.weight(1f))
                OpsStat("Succeeded", fleet.succeeded, TodaySuccess, Modifier.weight(1f))
                OpsStat("Failed", fleet.failed, Danger, Modifier.weight(1f))
                TaskPie(fleet, 48.dp)
            }
        } else {
            Column(Modifier.fillMaxSize(), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(14.dp)) {
                    TaskPie(fleet, 64.dp)
                    Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                        YieldRow(TodayWarning, "Active:", fleet.inFlight, fleet.inFlightPct)
                        YieldRow(TodaySuccess, "Succeeded:", fleet.succeeded, fleet.succeededPct)
                        YieldRow(Danger, "Failed:", fleet.failed, fleet.failedPct)
                    }
                }
                Row(horizontalArrangement = Arrangement.spacedBy(12.dp), verticalAlignment = Alignment.CenterVertically) {
                    AgentMetric("Enabled", agents?.enabled, Muted)
                    AgentMetric("Active", agents?.active, if ((agents?.active ?: 0) > 0) TodaySuccess else Muted)
                    AgentMetric("Total", agents?.total, Muted)
                    Spacer(Modifier.weight(1f))
                    Text("Tasks →", color = Coral, fontSize = 12.sp, fontWeight = FontWeight.SemiBold,
                        modifier = Modifier.clickable(onClick = onOpenTasks).padding(2.dp))
                }
                Hairline()
                if (tasks.isEmpty()) {
                    Text("No tasks yet today.", style = newsSerif(13.sp, FontWeight.Normal, italic = true, color = Secondary))
                } else {
                    Column(Modifier.fillMaxWidth().height(OPS_TASK_ROW * 3.5f).verticalScroll(rememberScrollState())) {
                        tasks.forEach { task -> TaskLine(task) { onOpenTask(task.id) } }
                    }
                }
            }
        }
    }
}

/** Expanded Economics: per-call cost, peak hour, coding runs, and memories (or evals when there are any). */
@Composable
private fun EconomicsStats(pulse: TodayPulse) {
    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        EconomicsStat("Avg / call", formatPerCall(avgCostPerCall(pulse.spendToday, pulse.callsToday)), Modifier.weight(1f))
        EconomicsStat("Peak hour", peakSpendHour(pulse.hourlySpend) ?: "—", Modifier.weight(1f))
        EconomicsStat("Coding runs", "${pulse.codingRunsToday}", Modifier.weight(1f))
        if (pulse.evalCasesToday > 0) EconomicsStat("Evals", "${pulse.evalPassesToday}/${pulse.evalCasesToday}", Modifier.weight(1f))
        else EconomicsStat("Memories", "${pulse.memoriesToday}", Modifier.weight(1f))
    }
}

@Composable
private fun EconomicsStat(label: String, value: String, modifier: Modifier) {
    Column(modifier.semantics(mergeDescendants = true) {}) {
        NewsKicker(label, color = Muted, size = 8.sp)
        Text(value, style = newsSerif(15.sp, FontWeight.Bold), maxLines = 1)
    }
}

/**
 * Slide 3, last 24 hours. Collapsed: crew totals (active now, cost, tasks
 * done, reliability). Expanded: the totals, then one row per agent (cost,
 * tasks, success, reliability), three and a half rows visible.
 */
@Composable
private fun StateOfTheCrew(crew: TodayCrew?, error: String?, expanded: Boolean, onRetry: () -> Unit) {
    if (crew == null) {
        if (error != null) TodayInlineError(error, onRetry)
        else Box(Modifier.fillMaxWidth().height(OPS_COLLAPSED_HEIGHT), contentAlignment = Alignment.Center) {
            CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp, color = Coral)
        }
        return
    }
    val totals: @Composable () -> Unit = {
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            OpsStat("Active", "${crew.activeNow}/${crew.total}", if (crew.activeNow > 0) TodaySuccess else Ink, Modifier.weight(1f))
            OpsStat("Cost 24h", formatPerCall(crew.costUsd.takeIf { crew.members.isNotEmpty() } ?: 0.0), Ink, Modifier.weight(1.1f))
            OpsStat("Tasks 24h", "${crew.tasksDone}", Ink, Modifier.weight(1f))
            OpsStat("Reliability", crew.reliabilityPct?.let { "$it%" } ?: "—", crewRateColor(crew.reliabilityPct), Modifier.weight(1.1f))
        }
    }
    AnimatedContent(
        targetState = expanded,
        transitionSpec = { fadeIn(tween(220, delayMillis = 60)) togetherWith fadeOut(tween(140)) },
        label = "stateOfCrew",
    ) { full ->
        if (!full) {
            totals()
        } else {
            Column(Modifier.fillMaxSize(), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                totals()
                Hairline()
                if (crew.members.isEmpty()) {
                    Text("The crew is resting — no activity in the last 24 hours.",
                        style = newsSerif(13.sp, FontWeight.Normal, italic = true, color = Secondary))
                } else {
                    CrewRow(null)
                    Column(Modifier.fillMaxWidth().height(OPS_TASK_ROW * 3.5f).verticalScroll(rememberScrollState())) {
                        crew.members.forEach { CrewRow(it) }
                    }
                }
            }
        }
    }
}

/** A crew table row; null draws the column header. */
@Composable
private fun CrewRow(member: TodayCrewMember?) {
    val mono = newsMono()
    Row(
        Modifier.fillMaxWidth().height(if (member == null) 16.dp else OPS_TASK_ROW)
            .semantics(mergeDescendants = true) {},
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        if (member == null) {
            Text("NAME", color = Muted, fontFamily = mono, fontSize = 8.sp, modifier = Modifier.weight(1f))
            listOf("COST" to 58, "TASKS" to 38, "SUCC." to 38, "REL." to 38).forEach { (label, width) ->
                Text(label, color = Muted, fontFamily = mono, fontSize = 8.sp, modifier = Modifier.width(width.dp), textAlign = TextAlign.End)
            }
            return@Row
        }
        Row(Modifier.weight(1f), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(7.dp)) {
            Box(Modifier.size(7.dp).background(
                when { member.active -> TodaySuccess; member.disabled -> BorderSoft; else -> Muted.copy(alpha = .5f) },
                RoundedCornerShape(50),
            ))
            Text(member.name, color = Ink, fontSize = 13.sp, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
        Text(formatPerCall(member.costUsd), color = Ink, fontFamily = mono, fontSize = 11.sp, modifier = Modifier.width(58.dp), textAlign = TextAlign.End, maxLines = 1)
        Text("${member.done}", color = Ink, fontFamily = mono, fontSize = 11.sp, modifier = Modifier.width(38.dp), textAlign = TextAlign.End)
        Text(member.successPct?.let { "$it%" } ?: "—", color = crewRateColor(member.successPct), fontFamily = mono, fontSize = 11.sp,
            modifier = Modifier.width(38.dp), textAlign = TextAlign.End)
        Text(member.reliabilityPct?.let { "$it%" } ?: "—", color = crewRateColor(member.reliabilityPct), fontFamily = mono, fontSize = 11.sp,
            modifier = Modifier.width(38.dp), textAlign = TextAlign.End)
    }
}

/** ≥ 95% success colour, 80–94 warning, below 80 danger; unknown stays muted. */
private fun crewRateColor(pct: Int?): Color = when {
    pct == null -> Muted
    pct >= 95 -> TodaySuccess
    pct >= 80 -> TodayWarning
    else -> Danger
}

@Composable
private fun OpsStat(label: String, value: String, color: Color, modifier: Modifier) {
    Column(modifier.semantics(mergeDescendants = true) {}) {
        FitText(value, style = newsSerif(24.sp, FontWeight.ExtraBold, color = color, lineHeight = 28.sp), minSize = 14.sp)
        NewsKicker(label, color = Muted, size = 9.sp)
    }
}

@Composable
private fun OpsStat(label: String, value: Int, color: Color, modifier: Modifier) {
    Column(modifier.semantics(mergeDescendants = true) {}) {
        Text("$value", style = newsSerif(28.sp, FontWeight.ExtraBold, color = color, lineHeight = 30.sp))
        NewsKicker(label, color = Muted, size = 9.sp)
    }
}

@Composable
private fun TaskLine(task: TodayTaskLine, onOpen: () -> Unit) {
    Row(
        Modifier.fillMaxWidth().height(OPS_TASK_ROW).clickable(onClick = onOpen),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Box(Modifier.size(7.dp).background(taskStatusColor(task.status), RoundedCornerShape(50)))
        Text(task.title, color = Ink, fontSize = 13.sp, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f))
        Text(taskShortAge(task.updatedAtMs), color = Muted, fontFamily = newsMono(), fontSize = 10.sp)
    }
}

private fun taskStatusColor(status: String): Color = when (status) {
    "completed", "done" -> TodaySuccess
    "failed" -> Danger
    "running", "paused", "planning" -> TodayWarning
    else -> BorderSoft
}

/** Solid task pie (succeeded, failed, in flight); a dashed IDLE disc when there are none. */
@Composable
private fun TaskPie(fleet: TodayFleetYield, size: androidx.compose.ui.unit.Dp) {
    val success = TodaySuccess; val danger = Danger; val warning = TodayWarning
    val neutralFill = Soft; val neutralStroke = BorderSoft; val surface = Panel
    Box(contentAlignment = Alignment.Center) {
        Canvas(
            Modifier.size(size).semantics {
                contentDescription = "Task yield: ${fleet.succeeded} succeeded, ${fleet.failed} failed, ${fleet.inFlight} active"
            },
        ) {
            val total = fleet.total
            if (total <= 0) {
                drawCircle(neutralFill)
                drawCircle(neutralStroke, style = Stroke(1.5.dp.toPx(), pathEffect = PathEffect.dashPathEffect(floatArrayOf(6f, 6f))))
            } else {
                var start = -90f
                listOf(fleet.succeeded to success, fleet.failed to danger, fleet.inFlight to warning)
                    .filter { it.first > 0 }
                    .forEach { (count, color) ->
                        val sweep = 360f * count / total
                        drawArc(color, start, sweep, useCenter = true)
                        if (sweep < 360f) drawArc(surface, start, sweep, useCenter = true, style = Stroke(1.5.dp.toPx()))
                        start += sweep
                    }
            }
        }
        if (fleet.total <= 0) Text("IDLE", color = Muted, fontFamily = newsMono(), fontSize = 9.sp, fontWeight = FontWeight.Bold)
    }
}

@Composable
private fun SpendPanel(pulse: TodayPulse, expanded: Boolean) {
    val (dollars, cents) = splitSpend(pulse.spendToday)
    val tone = spendTone(pulse.spendToday, pulse.spendYesterday)
    val toneColor = when (tone) {
        SpendTone.Good -> TodaySuccess
        SpendTone.Bad -> Danger
        SpendTone.Neutral -> Muted
    }
    val figure: @Composable (dollarSize: Int) -> Unit = { dollarSize ->
        Row(
            verticalAlignment = Alignment.Bottom,
            modifier = Modifier.semantics { contentDescription = "Spend today $$dollars$cents" },
        ) {
            Text("$", style = newsSerif((dollarSize * .45f).sp, FontWeight.Bold), modifier = Modifier.padding(bottom = (dollarSize * .4f).dp))
            Text(dollars, style = newsSerif(dollarSize.sp, FontWeight.ExtraBold, lineHeight = (dollarSize + 2).sp))
            if (cents.isNotEmpty()) Text(cents, style = newsSerif((dollarSize * .42f).sp, FontWeight.Bold, color = Secondary), modifier = Modifier.padding(bottom = (dollarSize * .18f).dp))
        }
    }
    // Collapsed: figure over "{n} calls" with the graph beside them, one
    // compact row. Expanded: the full ledger layout (provider, figure beside
    // the delta and call line, full-width graph). The two cross-fade while the
    // card resizes.
    AnimatedContent(
        targetState = expanded,
        transitionSpec = {
            (fadeIn(tween(220, delayMillis = 60)) togetherWith fadeOut(tween(140)))
                .using(SizeTransform(clip = true) { _, _ -> tween(280) })
        },
        label = "ledgerSpend",
    ) { full ->
        if (!full) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                Column {
                    figure(40)
                    Text(
                        "${String.format(Locale.US, "%,d", pulse.callsToday)} ${if (pulse.callsToday == 1) "call" else "calls"}",
                        color = Secondary, fontSize = 12.sp,
                    )
                }
                Box(Modifier.width(1.dp).height(56.dp).background(BorderSoft))
                Box(Modifier.weight(1f)) {
                    HourlySpendChart(pulse.hourlySpend, pulse.hourlyCalls, pulse.spendToday > 0, chartHeight = 44)
                }
            }
        } else {
            Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                NewsKicker(
                    pulse.topModel?.let { "TOP PROVIDER: ${it.provider} / ${it.model} (${topProviderShareLabel(it.share)}%)" }
                        ?: "COMMERCIAL MODEL EXPENDITURES",
                    color = Coral,
                )
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                    figure(44)
                    Box(Modifier.width(1.dp).height(44.dp).background(BorderSoft))
                    Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(3.dp)) {
                        Text(spendDeltaLine(pulse.spendToday, pulse.spendYesterday), color = toneColor, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
                        Text(
                            "${String.format(Locale.US, "%,d", pulse.callsToday)} model calls today",
                            color = Secondary, fontSize = 12.sp,
                        )
                    }
                }
                HourlySpendChart(pulse.hourlySpend, pulse.hourlyCalls, pulse.spendToday > 0, chartHeight = 92)
                EconomicsStats(pulse)
            }
        }
    }
}

@Composable
private fun HourlySpendChart(spend: List<Double>, calls: List<Int>, hasSpend: Boolean, chartHeight: Int = 52) {
    val accent = Coral
    val baseline = BorderSoft
    val currentHour = LocalTime.now().hour
    var selected by remember { mutableStateOf<Int?>(null) }
    val max = (spend.maxOrNull() ?: 0.0).coerceAtLeast(0.001)
    fun barAt(x: Float, width: Float): Int = ((x / width) * 24).toInt().coerceIn(0, 23)
    Column(verticalArrangement = Arrangement.spacedBy(3.dp)) {
        Canvas(
            Modifier.fillMaxWidth().height(chartHeight.dp)
                .semantics { contentDescription = "Hourly spend over 24 hours" }
                .pointerInput(Unit) { detectTapGestures { selected = barAt(it.x, size.width.toFloat()) } }
                .pointerInput(Unit) {
                    detectHorizontalDragGestures(
                        onDragEnd = {},
                        onHorizontalDrag = { change, _ -> selected = barAt(change.position.x, size.width.toFloat()) },
                    )
                },
        ) {
            val slot = size.width / 24f
            val barWidth = slot * .62f
            val floor = size.height - 2f
            val top = 4f
            val heights = (0 until 24).map { hour ->
                val value = spend.getOrElse(hour) { 0.0 }
                if (value > 0) maxOf(3.dp.toPx(), (value / max).toFloat() * (floor - top)) else 1.5.dp.toPx()
            }
            if (hasSpend) {
                val line = Path(); val area = Path()
                area.moveTo(slot / 2f, floor)
                heights.forEachIndexed { hour, height ->
                    val x = hour * slot + slot / 2f
                    val y = if (spend.getOrElse(hour) { 0.0 } > 0) floor - height else floor
                    if (hour == 0) line.moveTo(x, y) else line.lineTo(x, y)
                    area.lineTo(x, y)
                }
                area.lineTo(23 * slot + slot / 2f, floor); area.close()
                drawPath(area, Brush.verticalGradient(listOf(accent.copy(alpha = .28f), accent.copy(alpha = 0f))))
                drawPath(line, accent.copy(alpha = .75f), style = Stroke(width = 1.2.dp.toPx()))
            }
            drawLine(baseline, Offset(0f, floor), Offset(size.width, floor), strokeWidth = 1.dp.toPx())
            heights.forEachIndexed { hour, height ->
                val active = spend.getOrElse(hour) { 0.0 } > 0
                val color = when {
                    hour == selected -> accent
                    hour == currentHour -> accent.copy(alpha = .95f)
                    active -> accent.copy(alpha = .55f)
                    else -> baseline
                }
                drawRect(color, topLeft = Offset(hour * slot + (slot - barWidth) / 2f, floor - height), size = Size(barWidth, height))
                if (hour == currentHour) {
                    drawRect(accent, topLeft = Offset(hour * slot + (slot - barWidth) / 2f, floor + 1f), size = Size(barWidth, 1.5.dp.toPx()))
                }
            }
        }
        Row(Modifier.fillMaxWidth().clickable(enabled = selected != null) { selected = null }) {
            val hour = selected
            if (hour != null) {
                Text(
                    hourlySpendTooltip(hour, spend.getOrElse(hour) { 0.0 }, calls.getOrElse(hour) { 0 }),
                    color = Ink, fontFamily = newsMono(), fontSize = 10.sp, fontWeight = FontWeight.SemiBold,
                )
            } else {
                Text(hourLabel(0), color = Muted, fontFamily = newsMono(), fontSize = 9.sp)
                Spacer(Modifier.weight(1f))
                Text(hourLabel(12), color = Muted, fontFamily = newsMono(), fontSize = 9.sp)
                Spacer(Modifier.weight(1f))
                Text("NOW", color = Coral, fontFamily = newsMono(), fontSize = 9.sp, fontWeight = FontWeight.Bold)
            }
        }
    }
}

@Composable
private fun YieldRow(color: Color, label: String, count: Int, pct: Int) {
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(5.dp)) {
        Text("●", color = color, fontSize = 11.sp)
        Text(label, color = Secondary, fontSize = 12.sp, modifier = Modifier.weight(1f), maxLines = 1, overflow = TextOverflow.Ellipsis)
        Text("$count", color = Ink, fontSize = 12.sp, fontWeight = FontWeight.Bold)
        Text("($pct%)", color = Muted, fontSize = 10.sp)
    }
}

@Composable
private fun AgentMetric(label: String, value: Int?, dot: Color) {
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
        Text("●", color = dot, fontSize = 10.sp)
        Text("$label:", color = Secondary, fontSize = 11.sp)
        // Unknown (agents never answered) reads as a dash, not a made-up zero.
        Text(value?.toString() ?: "—", color = Ink, fontSize = 11.sp, fontWeight = FontWeight.Bold)
    }
}
