package ai.magicbeans.magdroid.today

import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.time.ZoneId
import java.time.ZonedDateTime

class TodayParityContractTest {
    private fun item(id: String = "one", section: TodaySection = TodaySection.FollowUps) = TodayItem(
        id = id, section = section.wire, title = "Review launch", sourceKind = "task",
        sourceId = "task-$id", updatedAt = 1_786_000_000_000,
    )

    @Test fun older_today_payloads_remain_readable() {
        val decoded = todayJson.decodeFromString<TodayResponse>(
            """{"headline":"Ready","sections":{"followups":[{"id":"f1","title":"Follow up"}]},"counts":{"followups":1}}""",
        )
        assertEquals("Ready", decoded.headline)
        assertEquals("f1", decoded.sections.followups.single().id)
        assertEquals("unknown", decoded.sections.followups.single().sourceKind)
        assertEquals(1, decoded.counts.followups)
    }

    @Test fun section_count_combines_channel_followups_but_not_other_lanes() {
        val state = TodayUiState(
            payload = TodayResponse(counts = TodayCounts(followups = 2, changed = 4)),
            messageFollowUpTotal = 3, resurfacingTotal = 5,
        )
        assertEquals(5, state.count(TodaySection.FollowUps))
        assertEquals(5, state.count(TodaySection.WorthALook))
        assertEquals(4, state.count(TodaySection.Changed))
    }

    @Test fun deck_paging_flags_follow_loaded_against_total() {
        val state = TodayUiState(
            messageFollowUps = listOf(ChannelFollowUp(annotationId = "a")), messageFollowUpTotal = 2,
            resurfacingCards = listOf(ResurfacingCard(candidateId = "w")), resurfacingTotal = 1,
        )
        assertTrue(state.hasMoreFollowUps)
        assertFalse(state.hasMoreResurfacing)
    }

    @Test fun source_action_accepts_only_post_today_endpoints() {
        val valid = TodayAction("run", "Run", "today_source_action", buildJsonObject {
            put("method", "POST"); put("endpoint", "/api/magician/v2/today/items/a/actions/run")
        })
        assertTrue(valid.isExecutableTodayAction)
        assertFalse(valid.copy(payload = buildJsonObject {
            put("method", "GET"); put("endpoint", "/api/magician/v2/today/items/a/actions/run")
        }).isExecutableTodayAction)
        assertFalse(valid.copy(payload = buildJsonObject {
            put("method", "POST"); put("endpoint", "/api/magician/v3/tasks")
        }).isExecutableTodayAction)
    }

    @Test fun today_action_navigation_accepts_all_three_server_envelopes() {
        assertEquals("direct", todayJson.decodeFromString<TodayActionExecutionResult>("""{"task_id":"direct"}""").resolvedTaskId)
        assertEquals("nav", todayJson.decodeFromString<TodayActionExecutionResult>("""{"navigate_to":{"task_id":"nav"}}""").resolvedTaskId)
        assertEquals("manifest", todayJson.decodeFromString<TodayActionExecutionResult>("""{"task":{"manifest":{"task_id":"manifest"}}}""").resolvedTaskId)
    }

    @Test fun attention_item_resolution_uses_server_identifiers_before_fallback() {
        assertEquals("pause-1", item().copy(metadata = buildJsonObject { put("pause_state_id", "pause-1") }).attentionItemId)
        assertEquals("selected-2", item().copy(sourceUrl = "https://magican/tasks?selected=selected-2").attentionItemId)
        assertEquals("selected/2", item().copy(sourceUrl = "/attention?selected=selected%2F2").attentionItemId)
        assertEquals("raw", item("today:followups:raw").attentionItemId)
    }

    @Test fun monitor_cards_preserve_the_exact_update_target() {
        val metadataTarget = item().copy(
            sourceKind = "monitor_update",
            metadata = buildJsonObject { put("monitor_task_id", "monitor-1"); put("update_id", "update-2") },
        ).monitorTarget
        assertEquals(TodayMonitorTarget("monitor-1", "update-2"), metadataTarget)
        assertEquals(
            TodayMonitorTarget("monitor 3", "update/4"),
            parseMonitorTasksRoute("/tasks?type=monitors&selected=monitor%203&update=update%2F4"),
        )
        assertNull(parseMonitorTasksRoute("/tasks?selected=task-1"))
    }

    @Test fun learned_items_are_bounded_and_tolerate_partial_records() {
        val metadata = todayJson.parseToJsonElement("""{"learned_items":[
          {"id":"1","title":"One"},{"summary":"Two"},{"title":"Three"},{"title":"Four"},{"title":"Five"},{}
        ]}""")
        val learned = item().copy(metadata = metadata).learnedItems
        assertEquals(4, learned.size)
        assertEquals(listOf("One", "Two", "Three", "Four"), learned.map(TodayLearnedItem::title))
    }

    @Test fun durable_activity_excludes_running_noise_and_empty_completions() {
        assertTrue(isDurableTodayActivity(TodayActivityItem("learn", itemType = "agent_learning")))
        assertTrue(isDurableTodayActivity(TodayActivityItem("failed", itemType = "task", status = "failed")))
        assertFalse(isDurableTodayActivity(TodayActivityItem("running", itemType = "task", status = "running")))
        assertFalse(isDurableTodayActivity(TodayActivityItem("empty", itemType = "task", status = "done")))
        assertTrue(isDurableTodayActivity(TodayActivityItem("done", itemType = "task", status = "done", summary = "Shipped")))
    }

    @Test fun optimistic_today_remove_and_rollback_restore_order_and_counts() {
        val first = item("first")
        val middle = item("middle")
        val last = item("last")
        val state = TodayUiState(payload = TodayResponse(
            sections = TodaySectionsPayload(followups = listOf(first, middle, last)),
            counts = TodayCounts(followups = 3, total = 3),
        ))
        val removed = removeTodayItem(state, middle)
        assertEquals(listOf("first", "last"), removed.items(TodaySection.FollowUps).map(TodayItem::id))
        assertEquals(2, removed.counts.total)
        val restored = insertTodayItem(removed, middle, TodaySection.FollowUps, 1)
        assertEquals(listOf("first", "middle", "last"), restored.items(TodaySection.FollowUps).map(TodayItem::id))
        assertEquals(3, restored.counts.followups)
    }

    @Test fun rollback_anchor_survives_other_cards_mutating_concurrently() {
        val anchor = CardAnchor.capture(listOf("a", "b", "c", "d"), "b")
        assertEquals(0, anchor.insertionIndex(listOf("c", "d")))
        assertEquals(1, anchor.insertionIndex(listOf("a", "d")))
        assertEquals(1, anchor.insertionIndex(listOf("a", "c", "d")))
    }

    @Test fun card_mutation_locks_cover_every_operation_key_for_that_card() {
        assertTrue(TodayUiState(pending = setOf("today:item-1")).isTodayItemMutationPending("item-1"))
        assertTrue(TodayUiState(pending = setOf("execute:item-1")).isTodayItemMutationPending("item-1"))
        assertTrue(TodayUiState(pending = setOf("followup:follow-1")).isFollowUpMutationPending("follow-1"))
        assertTrue(TodayUiState(pending = setOf("commit:follow-1:reply")).isFollowUpMutationPending("follow-1"))
        assertTrue(TodayUiState(pending = setOf("resurfacing:worth-1")).isResurfacingMutationPending("worth-1"))
        assertTrue(TodayUiState(pending = setOf("resurfacing-action:worth-1:create_task")).isResurfacingMutationPending("worth-1"))
        assertFalse(TodayUiState(pending = setOf("execute:item-2")).isTodayItemMutationPending("item-1"))
        assertFalse(TodayUiState(pending = setOf("commit:follow-10:reply")).isFollowUpMutationPending("follow-1"))
        assertFalse(TodayUiState(pending = setOf("resurfacing-action:worth-10:create_task")).isResurfacingMutationPending("worth-1"))
    }

    @Test fun activity_filters_and_search_stack() {
        val state = TodayUiState(
            activityItems = listOf(
                TodayActivityItem("learning", itemType = "agent_learning", title = "Learned tone"),
                TodayActivityItem("failed", itemType = "task", title = "Release failed", status = "failed"),
            ),
            activityFilter = TodayActivityFilter.Failed,
            activityQuery = "release",
        )
        assertEquals(listOf("failed"), state.filteredActivity().map(TodayActivityItem::id))
    }

    @Test fun snooze_targets_match_ios_local_calendar_rules() {
        val zone = ZoneId.of("Asia/Kolkata")
        val afternoon = ZonedDateTime.of(2026, 8, 10, 14, 0, 0, 0, zone)
        assertEquals(240, todaySnoozeMinutes(TodaySnoozeOption.Tonight, afternoon))
        val afterTonight = afternoon.withHour(20)
        assertEquals(180, todaySnoozeMinutes(TodaySnoozeOption.Tonight, afterTonight))
        assertEquals(720, todaySnoozeMinutes(TodaySnoozeOption.TomorrowMorning, afterTonight))
        assertTrue(todaySnoozeMinutes(TodaySnoozeOption.NextWeek, afternoon) > 0)
    }

    @Test fun delivery_validation_rejects_expired_or_mismatched_authority() {
        val reference = CanonicalAttentionProjectionReference("projection", "digest", "ready")
        val page = deliveryPage(expiresAt = 20_000)
        assertNotNull(page.validatedBindings(reference, "follow_up", "anonymous", "default", nowMs = 10_000))
        assertNull(page.validatedBindings(reference.copy(universeDigest = "other"), "follow_up", "anonymous", "default", nowMs = 10_000))
        assertNull(page.validatedBindings(reference, "follow_up", "anonymous", "default", nowMs = 20_000))
    }

    @Test fun canonical_delivery_reorders_only_an_exact_known_page() {
        val one = ChannelFollowUp(annotationId = "one")
        val two = ChannelFollowUp(annotationId = "two")
        val bindings = listOf(binding("two", 1), binding("one", 2))
        assertEquals(listOf("two", "one"), applyAttentionDelivery(bindings, listOf(one, two))?.map(ChannelFollowUp::id))
        assertNull(applyAttentionDelivery(bindings + binding("unknown", 3), listOf(one, two)))
    }

    @Test fun legacy_attribution_requires_selected_exact_route_and_revision() {
        val decision = AttentionDecisionBinding("decision", "candidate", "rev", "follow_up", selected = true)
        assertNotNull(validatedLegacyAttribution(decision, "candidate", "rev", "follow_up"))
        assertNull(validatedLegacyAttribution(decision, "candidate", "new", "follow_up"))
        assertNull(validatedLegacyAttribution(decision.copy(selected = false), "candidate", "rev", "follow_up"))
    }

    @Test fun realtime_refresh_is_family_and_scope_checked() {
        val relevant = """{"event_type":"TaskStatusChanged","data":{"principal":"anonymous","workspace":"default"}}"""
        assertTrue(isRelevantTodayEvent(relevant, "anonymous", "default"))
        assertFalse(isRelevantTodayEvent(relevant, "someone", "default"))
        assertFalse(isRelevantTodayEvent("""{"event_type":"ChatTokenDelta","data":{}}""", "anonymous", "default"))
        assertFalse(isRelevantTodayEvent("not-json", "anonymous", "default"))
    }

    @Test fun wire_frames_are_scope_checked_but_not_family_checked() {
        val chat = """{"event_type":"ChatMessageCreated","data":{"principal":"anonymous","workspace":"default"}}"""
        assertTrue(isScopedTodayEvent(chat, "anonymous", "default"))
        assertFalse(isRelevantTodayEvent(chat, "anonymous", "default"))
        assertFalse(isScopedTodayEvent(chat, "anonymous", "other"))
        assertFalse(isScopedTodayEvent("not-json", "anonymous", "default"))
    }

    @Test fun message_action_summary_preserves_owner_due_urgency_and_details() {
        val followUp = ChannelFollowUp(annotationId = "a", proposedAction = todayJson.parseToJsonElement("""{
          "follow_up_kind":"reply","action_owner":"me","due_text":"Today","urgency":"high",
          "key_details":["Price","Scope","Date","Ignored"]
        }"""))
        assertEquals("Reply · Owner: Me · Due: Today · High · Details: Price · Scope · Date", followUp.actionSummary)
    }

    private fun binding(rawId: String, position: Int) = AttentionDeliveryBinding(
        principal = "anonymous", workspace = "default", rawItemId = rawId, originKind = "follow_up",
        decisionId = "decision", deliveryId = "delivery", pageIndex = 0, position = position,
        exposureToken = "token-$position", candidateId = "candidate-$rawId", sourceRevision = null,
        surface = "follow_up", minVisibleMs = 500, visibilityRuleVersion = "v1",
        rootPolicyPropensity = .5, expiresAt = Long.MAX_VALUE,
    )

    private fun deliveryPage(expiresAt: Long) = AttentionDeliveryPageResponse(
        schemaVersion = 1,
        rootDecision = AttentionDeliveryRootDecision("decision", "follow_up", "projection", "digest", expiresAt),
        page = AttentionDeliveryPageIdentity("delivery", 0, 0, 1, expiresAt = expiresAt),
        items = listOf(AttentionDeliveredItem(
            position = 1, candidateId = "candidate", sourceRevision = "rev", rootPolicyPropensity = .5,
            conditionalDeliveryPropensity = 1.0, exposureToken = "token",
            item = AttentionDeliveredCanonicalItem("candidate", "rev", "follow_up", AttentionCanonicalOrigin("follow_up", annotationId = "annotation")),
        )),
        impressionPolicy = AttentionImpressionPolicy(500, "v1"),
    )
}
