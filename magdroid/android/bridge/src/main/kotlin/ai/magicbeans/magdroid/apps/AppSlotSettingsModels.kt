package ai.magicbeans.magdroid.apps

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.encodeToString
import java.util.UUID

/**
 * Client mirrors of the bounded slot settings/mutation contract.
 *
 * The settings read is also fence acquisition: every mutation must present the
 * exact revision and monotonic fence that read returned, so a stale editor —
 * another device, another tab, a picker left open across a package update —
 * can never overwrite a newer one. Nothing here is repaired or defaulted; a
 * page this client cannot fully re-derive is refused.
 */
@Serializable
data class AppSlotWriteHeadWire(
    val revision: Long,
    val fence: Long,
)

@Serializable
data class AppSlotSuggestionWire(
    val page: String,
    val region: String,
    @SerialName("slot_id") val slotId: String,
    @SerialName("system_default") val systemDefault: Boolean,
)

@Serializable
data class AppSlotPickerCandidateWire(
    val widget: AppSlotWidgetBindingWire,
    val title: String,
    @SerialName("suggested_slots") val suggestedSlots: List<AppSlotSuggestionWire>,
    @SerialName("system_class") val systemClass: Boolean,
)

@Serializable
data class AppSlotSettingsPageWire(
    val head: AppSlotWriteHeadWire,
    @SerialName("inventory_revision") val inventoryRevision: String,
    val assignments: List<AppResolvedSlotAssignmentWire>,
    @SerialName("next_assignment_cursor") val nextAssignmentCursor: String? = null,
    @SerialName("assignments_truncated") val assignmentsTruncated: Boolean,
    val picker: List<AppSlotPickerCandidateWire>,
    @SerialName("next_picker_cursor") val nextPickerCursor: String? = null,
    @SerialName("picker_truncated") val pickerTruncated: Boolean,
)

@Serializable
data class AppSlotAssignmentMutationReceiptWire(
    @SerialName("mutation_id") val mutationId: String,
    val head: AppSlotWriteHeadWire,
    val assignment: AppResolvedSlotAssignmentWire,
)

data class AppSlotWriteHead(val revision: Long, val fence: Long)

/** One assignable row of the host's inventory, exactly as it was read. */
data class AppSlotPickerCandidate(
    val binding: AppSlotWidgetBindingWire,
    val title: String,
    val suggestedSlotIds: Set<String>,
    val systemClass: Boolean,
) {
    val installationId: String get() = binding.packageIdentity.installationId
    val widgetId: String get() = binding.widgetId

    /**
     * Identity including revision, digest and generation.
     *
     * Two picker reads either side of a reinstall can name the same
     * installation and widget while binding different package content, so the
     * row a command names is matched on the full binding, never on the target.
     */
    val candidateKey: String = listOf(
        binding.packageIdentity.installationId,
        binding.widgetId,
        binding.packageIdentity.packageRevisionRef,
        binding.packageIdentity.packageContentDigest,
        binding.packageIdentity.installationGeneration.toString(),
    ).joinToString("\u0000")

    /** Uniqueness key for one page of picker rows. */
    val targetKey: String =
        binding.packageIdentity.installationId + "\u0000" + binding.widgetId
}

data class AppSlotSettingsPage(
    val head: AppSlotWriteHead,
    val inventoryRevision: String,
    val assignments: Map<String, AppResolvedSlotAssignmentWire>,
    val nextAssignmentCursor: String?,
    val assignmentsTruncated: Boolean,
    val picker: List<AppSlotPickerCandidate>,
    val nextPickerCursor: String?,
    val pickerTruncated: Boolean,
)

data class AppSlotSettingsQuery(
    val assignmentLimit: Int = AppSurfacingLimits.DefaultSlotAssignmentLimit,
    val assignmentCursor: String? = null,
    val pickerLimit: Int = AppSurfacingLimits.DefaultSlotPickerLimit,
    val pickerCursor: String? = null,
)

sealed interface AppSlotAssignmentCommand {
    val slotId: AppSlotId

    /** Pins the owner's own choice, bound to the exact row they saw. */
    data class Assign(
        override val slotId: AppSlotId,
        val candidate: AppSlotPickerCandidate,
    ) : AppSlotAssignmentCommand

    /**
     * Removes the widget from this slot for this owner. A pinned workspace
     * default survives, which is why restoring it is a separate command.
     */
    data class OptOut(override val slotId: AppSlotId) : AppSlotAssignmentCommand

    /** The only operation that clears this owner's customization of a slot. */
    data class RestoreWorkspaceDefault(override val slotId: AppSlotId) : AppSlotAssignmentCommand
}

data class AppSlotAssignmentWriteRequest(
    val expectedRevision: Long,
    val writeFence: Long,
    val mutationId: String,
    val command: AppSlotAssignmentCommand,
)

data class AppSlotAssignmentMutationReceipt(
    val mutationId: String,
    val head: AppSlotWriteHead,
    val assignment: AppResolvedSlotAssignment,
)

/**
 * A fresh identity for one intended change.
 *
 * The host replays a repeated mutation id rather than applying it twice, so
 * the id is minted once per intent and reused across an ambiguous retry.
 */
internal fun newAppSlotMutationId(): String = "slot-mutation:${UUID.randomUUID()}"

internal fun AppSlotSettingsQuery.checked(): AppSlotSettingsQuery {
    require(assignmentLimit in 1..AppSurfacingLimits.MaximumSlotAssignments)
    require(pickerLimit in 1..AppSurfacingLimits.MaximumPickerCandidates)
    listOfNotNull(assignmentCursor, pickerCursor).forEach { cursor ->
        require(cursor.isSlotCursor())
    }
    return this
}

internal fun AppSlotSettingsPageWire.checked(): AppSlotSettingsPage {
    require(head.revision >= 0)
    // The host's initial fence is zero and it refuses a write presenting it, so
    // a page handing back a zero fence cannot be mutated from at all.
    require(head.fence >= 1)
    require(inventoryRevision.isDigest())
    require(assignments.size <= AppSurfacingLimits.MaximumSlotAssignments)
    require(picker.size <= AppSurfacingLimits.MaximumPickerCandidates)
    require(assignmentsTruncated == (nextAssignmentCursor != null))
    require(pickerTruncated == (nextPickerCursor != null))
    listOfNotNull(nextAssignmentCursor, nextPickerCursor).forEach { cursor ->
        require(cursor.isSlotCursor())
    }
    val resolved = assignments.associate { assignment ->
        val slotId = canonicalAppSlotId(assignment.slotId)
        requireNotNull(slotId) { "slot settings named a non-canonical slot" }
        assignment.checked(slotId)
        slotId.value to assignment
    }
    require(resolved.size == assignments.size)
    val candidates = picker.map(AppSlotPickerCandidateWire::checked)
    require(candidates.map(AppSlotPickerCandidate::targetKey).toSet().size == candidates.size)
    return AppSlotSettingsPage(
        head = AppSlotWriteHead(head.revision, head.fence),
        inventoryRevision = inventoryRevision,
        assignments = resolved,
        nextAssignmentCursor = nextAssignmentCursor,
        assignmentsTruncated = assignmentsTruncated,
        picker = candidates,
        nextPickerCursor = nextPickerCursor,
        pickerTruncated = pickerTruncated,
    )
}

internal fun AppSlotPickerCandidateWire.checked(): AppSlotPickerCandidate {
    widget.packageIdentity.checked()
    require(widget.widgetId.isAppName())
    require(title.isBoundedText())
    require(suggestedSlots.size <= AppSurfacingLimits.MaximumSuggestedSlotsPerWidget)
    val suggested = suggestedSlots.map { suggestion ->
        val slotId = canonicalAppSlotId(suggestion.slotId)
        requireNotNull(slotId) { "picker suggested a non-canonical slot" }
        // The declared page/region must be the pair the id encodes; a
        // suggestion that says one thing and encodes another is refused.
        require(AppSlotId.forPageRegion(suggestion.page, suggestion.region) == slotId)
        // Only host-controlled provenance may claim a system default.
        require(!suggestion.systemDefault || systemClass)
        slotId.value
    }.toSet()
    require(suggested.size == suggestedSlots.size)
    return AppSlotPickerCandidate(widget, title, suggested, systemClass)
}

internal fun AppSlotAssignmentMutationReceiptWire.checked(
    request: AppSlotAssignmentWriteRequest,
): AppSlotAssignmentMutationReceipt {
    require(mutationId == request.mutationId)
    // Exactly one revision was consumed under the fence this client presented.
    // Anything else describes a write that is not this one.
    require(request.expectedRevision < Long.MAX_VALUE)
    require(head.revision == request.expectedRevision + 1 && head.fence == request.writeFence)
    val applied = assignment.checked(request.command.slotId)
    when (val command = request.command) {
        is AppSlotAssignmentCommand.Assign -> {
            require(applied.source == "user" && !applied.optedOut)
            val widget = requireNotNull(assignment.widget)
            require(widget.current == command.candidate.binding)
            require(widget.pinned == command.candidate.binding)
            require(!widget.restoredAcrossGeneration)
        }
        is AppSlotAssignmentCommand.OptOut ->
            require(applied.optedOut && assignment.widget == null && applied.source == null)
        is AppSlotAssignmentCommand.RestoreWorkspaceDefault ->
            require(!applied.optedOut && applied.source != "user")
    }
    return AppSlotAssignmentMutationReceipt(
        mutationId = mutationId,
        head = AppSlotWriteHead(head.revision, head.fence),
        assignment = applied,
    )
}

/**
 * The exact request body, flattened onto the host's internally tagged command.
 *
 * Optional members are omitted rather than sent as null, because the host
 * denies unknown fields per variant: an `opt_out` carrying a null
 * `installation_id` is a rejected request, not a tolerated one.
 */
internal fun slotMutationRequestJson(request: AppSlotAssignmentWriteRequest): String {
    require(request.expectedRevision >= 0 && request.writeFence >= 1)
    require(request.mutationId.startsWith("slot-mutation:") && request.mutationId.isReference())
    val wireCommand = when (val command = request.command) {
        is AppSlotAssignmentCommand.Assign -> {
            command.candidate.binding.packageIdentity.checked()
            require(command.candidate.binding.widgetId.isAppName())
            AppSlotAssignmentCommandWire(
                command = "assign",
                slotId = command.slotId.value,
                installationId = command.candidate.installationId,
                widgetId = command.candidate.widgetId,
                expectedCandidate = command.candidate.binding,
            )
        }
        is AppSlotAssignmentCommand.OptOut ->
            AppSlotAssignmentCommandWire(command = "opt_out", slotId = command.slotId.value)
        is AppSlotAssignmentCommand.RestoreWorkspaceDefault ->
            AppSlotAssignmentCommandWire(
                command = "restore_workspace_default",
                slotId = command.slotId.value,
            )
    }
    return appSurfacingJson.encodeToString(AppSlotAssignmentWriteRequestWire(
        expectedRevision = request.expectedRevision,
        writeFence = request.writeFence,
        mutationId = request.mutationId,
        command = wireCommand,
    ))
}

@Serializable
private data class AppSlotAssignmentCommandWire(
    val command: String,
    @SerialName("slot_id") val slotId: String,
    @SerialName("installation_id") val installationId: String? = null,
    @SerialName("widget_id") val widgetId: String? = null,
    @SerialName("expected_candidate") val expectedCandidate: AppSlotWidgetBindingWire? = null,
)

@Serializable
private data class AppSlotAssignmentWriteRequestWire(
    @SerialName("expected_revision") val expectedRevision: Long,
    @SerialName("write_fence") val writeFence: Long,
    @SerialName("mutation_id") val mutationId: String,
    val command: AppSlotAssignmentCommandWire,
)

private fun String.isSlotCursor(): Boolean =
    toByteArray().size in 1..AppSurfacingLimits.MaximumSlotCursorBytes &&
        none { Character.isISOControl(it) }
