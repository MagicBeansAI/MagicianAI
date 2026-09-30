package ai.magicbeans.magdroid.today

import kotlinx.serialization.json.JsonArray
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.time.LocalDate

class TodayMorningEditionTest {
    // ------------------------------------------------------------ Masthead

    @Test fun roman_numerals_are_standard_subtractive_with_a_floor_of_one() {
        assertEquals("I", romanNumeral(0))
        assertEquals("I", romanNumeral(-5))
        assertEquals("IV", romanNumeral(4))
        assertEquals("IX", romanNumeral(9))
        assertEquals("XIV", romanNumeral(14))
        assertEquals("XL", romanNumeral(40))
        assertEquals("MCMXCIV", romanNumeral(1994))
        assertEquals("MMXXVI", romanNumeral(2026))
    }

    @Test fun volume_counts_years_since_inception_and_issue_is_the_day_of_year() {
        assertEquals("I", morningEditionVolume(LocalDate.of(2023, 6, 1)))
        assertEquals("IV", morningEditionVolume(LocalDate.of(2026, 9, 28)))
        assertEquals(1, morningEditionIssue(LocalDate.of(2026, 1, 1)))
        assertEquals(271, morningEditionIssue(LocalDate.of(2026, 9, 28)))
        assertEquals(366, morningEditionIssue(LocalDate.of(2024, 12, 31)))
    }

    @Test fun masthead_dateline_and_volume() {
        assertEquals("MONDAY, SEPTEMBER 28, 2026", mastheadDateline(LocalDate.of(2026, 9, 28)))
        assertEquals("VOL. IV · NO. 271", mastheadVolume(LocalDate.of(2026, 9, 28)))
    }

    @Test fun greeting_matches_web_dayparts_and_late_night_reads_as_evening() {
        assertEquals("Good evening", todayGreeting(0))
        assertEquals("Good evening", todayGreeting(4))
        assertEquals("Good morning", todayGreeting(5))
        assertEquals("Good morning", todayGreeting(11))
        assertEquals("Good afternoon", todayGreeting(12))
        assertEquals("Good afternoon", todayGreeting(17))
        assertEquals("Good evening", todayGreeting(18))
        assertEquals("Good evening", todayGreeting(23))
    }

    // ------------------------------------------------------------ Wire

    @Test fun compact_24h_count_matches_web_formatting() {
        assertEquals("0/24H", formatCompact24h(0))
        assertEquals("0/24H", formatCompact24h(-3))
        assertEquals("412/24H", formatCompact24h(412))
        assertEquals("999/24H", formatCompact24h(999))
        assertEquals("1K/24H", formatCompact24h(1_000))
        assertEquals("1.5K/24H", formatCompact24h(1_500))
        assertEquals("35K/24H", formatCompact24h(35_086))
        assertEquals("2.4M/24H", formatCompact24h(2_400_000))
        assertEquals("12M/24H", formatCompact24h(12_000_000))
    }

    @Test fun event_types_humanize_with_acronyms_and_a_head() {
        assertEquals("Task: Status Changed", humanizeEventType("event.task.status_changed"))
        assertEquals("LLM Call", humanizeEventType("llm_call"))
        assertEquals("HITL: Request Created", humanizeEventType("hitl.request.created"))
        assertEquals("Task Status Changed", humanizeEventType("TaskStatusChanged"))
        assertEquals("System event", humanizeEventType("event."))
        assertEquals("System event", humanizeEventType(""))
    }

    @Test fun feed_rows_become_insight_or_activity_lines_with_fallbacks() {
        val insight = normalizeFeedWireItem(TodayActivityItem("f1", itemType = "agent_learning", title = " ", updatedAt = 5), nowMs = 9)
        assertEquals("feed-f1", insight.id)
        assertEquals(TodayWireKind.Insight, insight.kind)
        assertEquals("Distilled Memory", insight.title)
        assertEquals("Feed insight recorded", insight.summary)
        assertEquals("agent learning", insight.badge)
        assertEquals(5, insight.timestamp)

        val activity = normalizeFeedWireItem(TodayActivityItem("f2", itemType = "task", title = "", taskId = "t-9", status = "failed"), nowMs = 9)
        assertEquals(TodayWireKind.Activity, activity.kind)
        assertEquals("Fleet Activity", activity.title)
        assertEquals("Task t-9", activity.summary)
        assertEquals(TodayWireSeverity.Error, activity.severity)
        assertEquals("t-9", activity.taskId)
        assertEquals(9, activity.timestamp)
        assertEquals(TodayWireSeverity.Success, normalizeFeedWireItem(TodayActivityItem("f3", status = "done")).severity)
    }

    @Test fun agent_updates_parse_tolerantly_and_normalize() {
        val updates = parseAgentUpdates(todayJson.parseToJsonElement("""{"events":[
          {"id":"u1","agent_id":"research-bot","kind":"cycle_failed","ts":42,"error":"boom","outcome":"failed","thread_id":"th"},
          {"id":"u2","kind":"cycle_started","ts":43,"focus_area":"inbox"},
          {"id":"u3","kind":"idle","ts":44},
          {"kind":"missing id"}
        ]}"""))
        assertEquals(listOf("u1", "u2", "u3"), updates.map(TodayAgentUpdate::id))
        val failed = normalizeAgentUpdate(updates[0])
        assertEquals("agent-update-u1", failed.id)
        assertEquals("Research Bot: cycle failed", failed.title)
        assertEquals("boom", failed.summary)
        assertEquals(TodayWireSeverity.Error, failed.severity)
        assertEquals("th", failed.threadId)
        assertEquals("Fleet Agent: cycle started", normalizeAgentUpdate(updates[1]).title)
        assertEquals("inbox", normalizeAgentUpdate(updates[1]).summary)
        assertEquals("Autonomous agent cycle logged", normalizeAgentUpdate(updates[2]).summary)
        assertTrue(parseAgentUpdates(todayJson.parseToJsonElement("[]")).isEmpty())
    }

    @Test fun realtime_frames_become_event_lines_preferring_payload_text() {
        val titled = normalizeRealtimeWireEvent(
            """{"event_type":"task.status.changed","timestamp_ms":1000,"data":{"task_id":"t1","payload":{"title":"Deploy","outcome":"completed"}}}""",
        )
        assertNotNull(titled)
        assertEquals(TodayWireKind.Event, titled!!.kind)
        assertEquals("Deploy", titled.title)
        assertEquals("Task t1", titled.summary)
        assertEquals(TodayWireSeverity.Success, titled.severity)
        assertEquals(1000, titled.timestamp)
        assertEquals("t1", titled.taskId)

        val failed = normalizeRealtimeWireEvent("""{"event_type":"execution.failed","data":{"error":"exit 1"}}""", nowMs = 7)!!
        assertEquals("Execution: Failed", failed.title)
        assertEquals("exit 1", failed.summary)
        assertEquals(TodayWireSeverity.Error, failed.severity)
        assertEquals(7, failed.timestamp)

        val iso = normalizeRealtimeWireEvent("""{"event_type":"memory.saved","timestamp":"2026-09-28T00:00:00Z","data":{"message":"Saved"}}""")!!
        assertEquals(1_790_553_600_000, iso.timestamp)
        assertEquals("Saved", iso.summary)

        val wrapped = normalizeRealtimeWireEvent(
            """{"event_type":"AgentEvent","data":{"event":{"event_type":"tool.call.started","agent_id":"personal-assistant","payload":{"event_id":"evt-1","tool_name":"read_file"},"timestamp":1700000000000}}}""",
            nowMs = 9,
        )!!
        assertEquals("Personal Assistant: Tool: Call Started", wrapped.title)
        assertEquals("event-evt-1", wrapped.id)
        assertEquals(1_700_000_000_000L, wrapped.timestamp)
        assertNull(normalizeRealtimeWireEvent("""{"event_type":"Heartbeat","data":{}}"""))
        assertNull(normalizeRealtimeWireEvent("""{"event_type":"ChatTokenDelta","data":{}}"""))
        assertNull(normalizeRealtimeWireEvent("not json"))
        assertNull(normalizeRealtimeWireEvent("""{"data":{}}"""))
    }

    @Test fun wire_merge_dedupes_sorts_newest_first_and_caps_at_fifty() {
        fun line(id: String, ts: Long, title: String = id) = TodayWireItem(id, TodayWireKind.Event, title, "", ts, "")
        val merged = mergeWireItems(listOf(line("a", 1), line("b", 3)), listOf(line("a", 5, "newer"), line("c", 2)))
        assertEquals(listOf("a", "b", "c"), merged.map(TodayWireItem::id))
        assertEquals("newer", merged.first().title)
        val many = mergeWireItems(emptyList(), (1..80).map { line("x$it", it.toLong()) })
        assertEquals(TODAY_WIRE_MAX_ITEMS, many.size)
        assertEquals("x80", many.first().id)
    }

    @Test fun wire_filters_show_five_and_count_like_web() {
        val items = (1..4).map { TodayWireItem("e$it", TodayWireKind.Event, "", "", it.toLong(), "") } +
            (1..3).map { TodayWireItem("i$it", TodayWireKind.Insight, "", "", it.toLong(), "") }
        assertEquals(5, filterWireItems(items, TodayWireFilter.All).size)
        assertEquals(3, filterWireItems(items, TodayWireFilter.Insights).size)
        assertEquals(5, wireFilterCount(items, TodayWireFilter.All))
        assertEquals(4, wireFilterCount(items, TodayWireFilter.Events))
        assertEquals(0, wireFilterCount(items, TodayWireFilter.Activity))
    }

    @Test fun event_count_query_reads_the_first_cell() {
        assertEquals(35_086L, parseEventCount24h(todayJson.parseToJsonElement("""{"columns":["total_24h"],"rows":[[35086]]}""")))
        assertEquals(12L, parseEventCount24h(todayJson.parseToJsonElement("""{"rows":[[12.0]]}""")))
        assertNull(parseEventCount24h(todayJson.parseToJsonElement("""{"rows":[]}""")))
        assertEquals("SELECT COUNT(*) AS total_24h FROM events WHERE epoch_ms(timestamp) >= 13600000", eventCount24hSql(100_000_000))
    }

    @Test fun wire_time_ago_uses_the_web_thresholds() {
        assertEquals("just now", wireTimeAgo(0, 44_000))
        assertEquals("1m ago", wireTimeAgo(0, 60_000))
        assertEquals("2h ago", wireTimeAgo(0, 7_200_000))
        assertEquals("1d ago", wireTimeAgo(0, 86_400_000))
    }

    // ------------------------------------------------------------ Ledger

    @Test fun spend_formats_and_splits_for_the_big_figure() {
        assertEquals("$0.00", formatSpend(0.0))
        assertEquals("$12.30", formatSpend(12.3))
        assertEquals("$123", formatSpend(123.4))
        assertEquals("0" to ".42", splitSpend(0.42))
        assertEquals("123" to "", splitSpend(123.4))
    }

    @Test fun spend_delta_and_inverted_tone() {
        assertEquals("", formatSpendDelta(0.0, 0.0))
        assertEquals("new today", formatSpendDelta(1.0, 0.0))
        assertEquals("", formatSpendDelta(1.002, 1.0))
        assertEquals("+$0.50", formatSpendDelta(1.5, 1.0))
        assertEquals("−$0.50", formatSpendDelta(1.0, 1.5))
        assertEquals(SpendTone.Neutral, spendTone(0.0, 0.0))
        assertEquals(SpendTone.Bad, spendTone(1.0, 0.0))
        assertEquals(SpendTone.Neutral, spendTone(1.002, 1.0))
        assertEquals(SpendTone.Bad, spendTone(2.0, 1.0))
        assertEquals(SpendTone.Good, spendTone(1.0, 2.0))
        assertEquals("steady vs $0.00 yday", spendDeltaLine(0.0, 0.0))
        assertEquals("+$0.50 vs $1.00 yday", spendDeltaLine(1.5, 1.0))
    }

    @Test fun provider_share_hour_labels_and_tooltip() {
        assertEquals("50", topProviderShareLabel(.5))
        assertEquals("33.33", topProviderShareLabel(1.0 / 3))
        assertEquals(listOf("12a", "1a", "11a", "12p", "3p", "11p"), listOf(0, 1, 11, 12, 15, 23).map(::hourLabel))
        assertEquals("3p: $0.42 (12 calls)", hourlySpendTooltip(15, .42, 12))
        assertEquals("12a: $0.00 (1 call)", hourlySpendTooltip(0, 0.0, 1))
    }

    @Test fun task_buckets_and_yield_percentages() {
        val rows = todayJson.parseToJsonElement("""[
          {"status":"completed"},{"status":"completed"},{"status":"done"},{"status":"failed"},
          {"status":"running"},{"status":"paused"},{"status":"planning"},{"status":"ready"}
        ]""") as JsonArray
        assertEquals(TodayTaskBuckets(completed = 2, failed = 1, inFlight = 3), taskBuckets(rows))

        val even = todayFleetYield(TodayTaskBuckets(1, 1, 1), 0)
        assertEquals(listOf(33, 33, 34), listOf(even.succeededPct, even.failedPct, even.inFlightPct))
        val pulseWins = todayFleetYield(TodayTaskBuckets(1, 1, 0), pulseCompletedToday = 3)
        assertEquals(3, pulseWins.succeeded)
        assertEquals(listOf(75, 25, 0), listOf(pulseWins.succeededPct, pulseWins.failedPct, pulseWins.inFlightPct))
        val idle = todayFleetYield(TodayTaskBuckets(), 0)
        assertEquals(0, idle.total)
        assertEquals(listOf(0, 0, 0), listOf(idle.succeededPct, idle.failedPct, idle.inFlightPct))
    }

    @Test fun agent_counts_use_the_crew_not_system_agents() {
        val counts = parseAgentCounts(todayJson.parseToJsonElement("""{
          "agents":[{"status":"running"},{"status":"triggered"},{"status":"idle","disabled":true},{"status":"disabled"},{"status":"idle"}],
          "system_agents":[{"status":"running"}]
        }"""))
        assertEquals(TodayAgentCounts(total = 5, enabled = 3, active = 2), counts)
        assertEquals(TodayAgentCounts(), parseAgentCounts(todayJson.parseToJsonElement("{}")))
    }

    // ------------------------------------------------------------ Reading room

    @Test fun deck_interleaves_two_dispatches_per_reading_room_card() {
        val followUps = listOf("a", "b", "c", "d", "e").map { ChannelFollowUp(annotationId = it) }
        val worth = listOf("x", "y").map { ResurfacingCard(candidateId = it) }
        assertEquals(
            listOf("followup:a", "followup:b", "worth:x", "followup:c", "followup:d", "worth:y", "followup:e"),
            interleaveDeck(followUps, worth).map(TodayDeckCard::id),
        )
        assertEquals(listOf("worth:x", "worth:y"), interleaveDeck(emptyList(), worth).map(TodayDeckCard::id))
        assertTrue(interleaveDeck(emptyList(), emptyList()).isEmpty())
    }

    @Test fun deck_cards_use_web_titles_categories_and_fallbacks() {
        val bare = deckCard(ChannelFollowUp(annotationId = "a", provider = ""))
        assertEquals("Untitled Message", bare.title)
        assertEquals("DISPATCH · CORRESPONDENCE", bare.category)
        assertEquals("No preview available", bare.summary)
        assertEquals("Do it", bare.primaryLabel)
        val gmail = deckCard(ChannelFollowUp(annotationId = "b", provider = "gmail", subject = "Invoice", reason = "Due today", sender = "Ana"))
        assertEquals("DISPATCH · GMAIL", gmail.category)
        assertEquals("Due today", gmail.summary)
        assertEquals("Ana", gmail.sender)

        val note = deckCard(ResurfacingCard(candidateId = "w", sourceKind = "project_note", line = "Line", whyNow = "Because"))
        assertEquals("READING ROOM · PROJECT NOTE", note.category)
        assertEquals("Line", note.title)
        assertEquals("Because", note.summary)
        assertEquals("Open", note.primaryLabel)
        val empty = deckCard(ResurfacingCard(candidateId = "v", sourceKind = ""))
        assertEquals("Resurfaced Note", empty.title)
        assertEquals("READING ROOM · NOTE", empty.category)
        assertEquals("Resurfaced for your attention", empty.summary)
    }

    @Test fun deck_tabs_track_remaining_and_triaged_cards() {
        val state = TodayUiState(
            messageFollowUps = listOf("a", "b").map { ChannelFollowUp(annotationId = it) },
            resurfacingCards = listOf(ResurfacingCard(candidateId = "w")),
            deckTriaged = mapOf("followup:a" to TodayDeckLane.Dispatch, "worth:gone" to TodayDeckLane.ReadingRoom),
        )
        assertEquals(listOf("followup:b", "worth:w"), state.deckStack(TodayDeckTab.All).map(TodayDeckCard::id))
        assertEquals(listOf("followup:b"), state.deckStack(TodayDeckTab.ForYou).map(TodayDeckCard::id))
        assertEquals(listOf("worth:w"), state.deckStack(TodayDeckTab.Worth).map(TodayDeckCard::id))
        assertEquals(2, state.deckTriagedCount(TodayDeckTab.All))
        assertEquals(1, state.deckTriagedCount(TodayDeckTab.ForYou))
        assertEquals(1, state.deckTriagedCount(TodayDeckTab.Worth))
    }

    @Test fun reading_room_count_mode_and_paging_rules() {
        assertEquals("0", readingRoomCountLabel(0))
        assertEquals("50", readingRoomCountLabel(50))
        assertEquals("50+", readingRoomCountLabel(51))
        assertEquals(TodayReadingRoomMode.Deck, TodayReadingRoomMode.fromWire(null))
        assertEquals(TodayReadingRoomMode.Broadsheet, TodayReadingRoomMode.fromWire("broadsheet"))
        assertTrue(deckNeedsMore(2, hasMoreFollowUps = true, hasMoreWorth = false))
        assertFalse(deckNeedsMore(3, hasMoreFollowUps = true, hasMoreWorth = true))
        assertFalse(deckNeedsMore(0, hasMoreFollowUps = false, hasMoreWorth = false))
    }

    @Test fun resurfacing_dismiss_reasons_exclude_classifier_corrections() {
        assertEquals(setOf("spam", "already_handled", "duplicate", "delegated", "not_relevant"), ChannelDismissOption.resurfacingCodes)
        assertTrue(ChannelDismissOption.all.any { it.code == "wrong_classification" })
    }

    @Test fun deliverable_dateline_and_briefing_meta_list_only_present_parts() {
        assertEquals("FILED · ROUTINE RESULT", deliverableDateline(TodayItem(id = "d", sourceKind = "routine_result")))
        val briefing = TodayBriefing(
            surface = TodayBriefingSurface("s", "/briefing", "Report", taskId = "t1", publishedAt = "2026-09-28T00:00:00Z"),
            sourceAgentId = "scout",
        )
        assertEquals("By scout · Task t1 · 2h ago", briefingMeta(briefing, nowMs = 1_790_553_600_000 + 7_200_000))
        assertEquals("", briefingMeta(briefing.copy(sourceAgentId = null, surface = briefing.surface.copy(taskId = null, publishedAt = "bad"))))
    }

    @Test fun worth_cards_decode_their_source_links() {
        val card = todayJson.decodeFromString<ResurfacingCard>(
            """{"candidate_id":"c","source_route":"/t/abc","open_url":"https://example.test/x"}""",
        )
        assertEquals("/t/abc", card.sourceRoute)
        assertEquals("https://example.test/x", card.openUrl)
    }

    @Test fun broadsheet_pages_are_five_wide_with_web_range_labels() {
        assertEquals(1, broadsheetPageCount(0))
        assertEquals(1, broadsheetPageCount(5))
        assertEquals(5, broadsheetPageCount(23))
        val second = TodayBroadsheetPage(page = 2, items = listOf("f", "g", "h", "i", "j"), total = 23, loaded = true)
        assertEquals("6–10 of 23", broadsheetRangeLabel(second))
        assertTrue(second.hasPrevious); assertTrue(second.hasNext)
        val last = TodayBroadsheetPage(page = 5, items = listOf("v", "w", "x"), total = 23, loaded = true)
        assertEquals("21–23 of 23", broadsheetRangeLabel(last))
        assertFalse(last.hasNext)
        assertEquals("0 of 0", broadsheetRangeLabel(TodayBroadsheetPage<String>(loaded = true)))
    }

    @Test fun removing_a_card_from_a_broadsheet_page_drops_the_total() {
        val page = TodayBroadsheetPage(page = 1, items = listOf("a", "b"), total = 7, loaded = true)
        val without = page.without("a") { it }
        assertEquals(listOf("b"), without.items)
        assertEquals(6, without.total)
        assertEquals(page, page.without("zzz") { it })
    }

    @Test fun recent_task_lines_are_newest_first_and_capped() {
        val rows = todayJson.parseToJsonElement(
            """[{"id":"a","title":"Old","status":"completed","updated_at":"2026-09-27T04:00:00Z"},
               {"id":"b","title":" ","status":"failed","updated_at":"2026-09-28T06:00:00Z"},
               {"id":"","title":"No id","status":"running","updated_at":"2026-09-28T07:00:00Z"},
               {"id":"c","title":"Mid","status":"running","updated_at":"2026-09-28T05:00:00Z"}]""",
        ) as kotlinx.serialization.json.JsonArray
        val lines = recentTaskLines(rows)
        assertEquals(listOf("b", "c", "a"), lines.map(TodayTaskLine::id))
        assertEquals("Untitled task", lines.first().title)
        assertEquals(listOf("b", "c"), recentTaskLines(rows, limit = 2).map(TodayTaskLine::id))
    }

    @Test fun task_short_age_is_compact() {
        val now = 1_000_000_000L
        assertEquals("now", taskShortAge(now - 20_000, now))
        assertEquals("4m", taskShortAge(now - 4 * 60_000, now))
        assertEquals("2h", taskShortAge(now - 2 * 3_600_000, now))
        assertEquals("3d", taskShortAge(now - 3 * 86_400_000L, now))
        assertEquals("", taskShortAge(0, now))
    }

    @Test fun carousel_wraps_and_holds_after_manual_navigation() {
        assertEquals(1, nextCarouselIndex(0, 2))
        assertEquals(0, nextCarouselIndex(1, 2))
        assertEquals(0, nextCarouselIndex(3, 0))
        assertTrue(carouselMayAdvance(nowMs = 100, pausedUntilMs = 100, dragging = false, reduceMotion = false))
        assertFalse(carouselMayAdvance(nowMs = 99, pausedUntilMs = 100, dragging = false, reduceMotion = false))
        assertFalse(carouselMayAdvance(nowMs = 200, pausedUntilMs = 0, dragging = true, reduceMotion = false))
        assertFalse(carouselMayAdvance(nowMs = 200, pausedUntilMs = 0, dragging = false, reduceMotion = true))
    }

    @Test fun economics_stats_match_web() {
        assertEquals(0.04 / 27, avgCostPerCall(0.04, 27)!!, 1e-9)
        assertNull(avgCostPerCall(1.0, 0))
        assertEquals("$0.0015", formatPerCall(0.0014814))
        assertEquals("$0.25", formatPerCall(0.25))
        assertEquals("—", formatPerCall(null))
        val hours = MutableList(24) { 0.0 }
        assertNull(peakSpendHour(hours))
        hours[12] = 0.02; hours[3] = 0.01
        assertEquals("12p", peakSpendHour(hours))
        hours[15] = 0.05
        assertEquals("3p", peakSpendHour(hours))
    }

    @Test fun crew_joins_agents_usage_and_tasks_active_first() {
        val agents = parseCrewAgents(todayJson.parseToJsonElement(
            """{"agents":[
                {"status":"idle","definition":{"agent_id":"personal-assistant","name":"Presto"}},
                {"status":"running","definition":{"agent_id":"cto","name":"CTO"}},
                {"status":"disabled","definition":{"agent_id":"quiet","name":"Quiet"}}]}""",
        ))
        assertEquals(listOf("Presto", "CTO", "Quiet"), agents.map(TodayCrewAgent::name))
        val usage = parseCrewUsage(
            listOf("agent_id", "calls", "cost_usd", "ok_calls"),
            listOf(
                todayJson.parseToJsonElement("""["personal-assistant", 22, 0.0625, 21]""") as kotlinx.serialization.json.JsonArray,
                todayJson.parseToJsonElement("""["ghost", "3", "0.001", "3"]""") as kotlinx.serialization.json.JsonArray,
            ),
        )
        val since = 1_000L
        val tasks = listOf(
            TodayCrewTask("personal-assistant", "completed", 2_000),
            TodayCrewTask("personal-assistant", "completed", 2_000),
            TodayCrewTask("personal-assistant", "failed", 2_000),
        )
        val crew = assembleCrew(agents, usage, tasks)
        // CTO is running (active first) though it spent nothing; Quiet had no activity and is left out.
        assertEquals(listOf("cto", "personal-assistant", "ghost"), crew.members.map(TodayCrewMember::id))
        val presto = crew.members.first { it.id == "personal-assistant" }
        assertEquals(67, presto.successPct)
        assertEquals(95, presto.reliabilityPct)
        assertNull(crew.members.first { it.id == "cto" }.successPct)
        assertEquals(1, crew.activeNow)
        assertEquals(3, crew.total)
        assertEquals(2, crew.tasksDone)
        assertEquals(96, crew.reliabilityPct)
        assertTrue(since < 2_000)
    }

    @Test fun crew_task_window_and_page_walk() {
        val rows = todayJson.parseToJsonElement(
            """[{"agent_id":"a","status":"completed","updated_at":"2026-09-28T06:00:00Z"},
               {"agent_id":"","status":"completed","updated_at":"2026-09-28T05:00:00Z"},
               {"agent_id":"b","status":"failed","updated_at":"2026-09-26T05:00:00Z"}]""",
        ) as kotlinx.serialization.json.JsonArray
        val since = java.time.Instant.parse("2026-09-27T07:00:00Z").toEpochMilli()
        assertEquals(listOf("a"), crewTasks(rows, since).map(TodayCrewTask::agentId))
        assertFalse(crewTaskPageContinues(rows, since, "next"))
        val fresh = todayJson.parseToJsonElement("""[{"agent_id":"a","updated_at":"2026-09-28T06:00:00Z"}]""") as kotlinx.serialization.json.JsonArray
        assertTrue(crewTaskPageContinues(fresh, since, "next"))
        assertFalse(crewTaskPageContinues(fresh, since, null))
        assertTrue(crewUsageSql(42).contains("timestamp_ms >= 42"))
    }
}
