package ai.magicbeans.magdroid.vibedev

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import java.util.UUID

/** What a sheet submission led to, for the screen to act on. */
sealed class VibeDevComposed {
    data class RunStarted(val taskId: String, val followUp: Boolean, val replayed: Boolean, val title: String) : VibeDevComposed()
    data class CrewProposed(val crew: VibeDevCrew, val projectId: String) : VibeDevComposed()
    data class ProjectCreated(val project: VibeDevProject) : VibeDevComposed()
}

data class VibeDevComposerState(
    val profiles: List<VibeDevCodingProfile> = emptyList(),
    val profilesFailed: Boolean = false,
    val repos: List<VibeDevRepoSuggestion> = emptyList(),
    val submitting: Boolean = false,
    val error: String? = null,
    val result: VibeDevComposed? = null,
)

/** The New run, Follow up and New project sheets. */
class VibeDevComposerViewModel(
    private val source: () -> VibeDevSource,
    private val newId: () -> String = { UUID.randomUUID().toString() },
) : ViewModel() {
    private val mutable = MutableStateFlow(VibeDevComposerState())
    val state: StateFlow<VibeDevComposerState> = mutable.asStateFlow()

    /**
     * One id per submission, kept across retries of the same submission so a
     * resend returns the run already started; a new sheet mints a new one.
     */
    private var clientRunId: String = newId()

    fun open() {
        clientRunId = newId()
        mutable.value = mutable.value.copy(error = null, result = null, submitting = false)
        if (mutable.value.profiles.isEmpty()) loadProfiles()
    }

    fun openProject() {
        mutable.value = mutable.value.copy(error = null, result = null, submitting = false)
        viewModelScope.launch {
            runCatching { source().repoSuggestions() }.getOrNull()?.let { repos ->
                mutable.value = mutable.value.copy(repos = repos)
            }
        }
    }

    private fun loadProfiles() {
        viewModelScope.launch {
            val profiles = runCatching { source().codingProfiles() }.getOrNull()
            mutable.value = mutable.value.copy(
                profiles = profiles.orEmpty().filter { it.selectable },
                profilesFailed = profiles == null,
            )
        }
    }

    fun startRun(
        prompt: String,
        projectId: String,
        mode: String,
        parentTaskId: String?,
        codingProfileId: String?,
        budgetUsd: Double?,
        location: String? = null,
        attachments: List<ai.magicbeans.magdroid.chat.StagedAttachment> = emptyList(),
        attachmentSessionId: String? = null,
        nightly: Boolean = false,
    ) = submit {
        if (parentTaskId == null && location != null) updateProject(projectId, null, null, location)
        val started = startRun(
            VibeDevRunRequest(
                prompt = prompt.trim(),
                projectId = projectId,
                mode = mode,
                clientRunId = clientRunId,
                parentTaskId = parentTaskId,
                codingChoice = codingModelSelection(mutable.value.profiles, codingProfileId),
                costBudgetUsd = budgetUsd,
                attachments = attachments, attachmentSessionId = attachmentSessionId, nightly = nightly,
            ),
        )
        val title = prompt.trim().lineSequence().first().take(60)
        VibeDevComposed.RunStarted(started.taskId, parentTaskId != null, started.replayed, title)
    }

    fun proposeCrew(projectId: String, goal: String, race: Boolean, attempts: Int, review: Boolean, budgetUsd: Double?) = submit {
        val crew = createCrew(
            VibeDevCrewRequest(
                projectId = projectId,
                goal = goal.trim(),
                mode = if (race) "race" else "split",
                racers = if (race) attempts.coerceIn(2, 8) else null,
                review = review,
                budgetUsd = budgetUsd,
            ),
        )
        VibeDevComposed.CrewProposed(crew, projectId)
    }

    fun createProject(name: String, repoPath: String, trusted: Boolean, baseBranch: String?, location: String? = null) = submit {
        var project = createProject(name.trim(), repoPath.trim())
        val base = baseBranch?.trim()?.takeIf { it.isNotEmpty() }
        if (trusted || base != null || location != null) project = updateProject(project.projectId, if (trusted) true else null, base, location)
        VibeDevComposed.ProjectCreated(project)
    }

    fun consumeResult() { mutable.value = mutable.value.copy(result = null) }

    private fun submit(block: suspend VibeDevSource.() -> VibeDevComposed) {
        if (mutable.value.submitting) return
        mutable.value = mutable.value.copy(submitting = true, error = null)
        viewModelScope.launch {
            try {
                val result = source().block()
                clientRunId = newId()
                mutable.value = mutable.value.copy(submitting = false, result = result)
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (error: Exception) {
                mutable.value = mutable.value.copy(submitting = false, error = error.message ?: "That did not go through.")
            }
        }
    }
}
