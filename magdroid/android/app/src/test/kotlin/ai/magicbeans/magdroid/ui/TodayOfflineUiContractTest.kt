package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.net.Failure
import ai.magicbeans.magdroid.net.FailureKind
import ai.magicbeans.magdroid.today.TodayUiState
import org.junit.Assert.assertEquals
import org.junit.Test

class TodayOfflineUiContractTest {
    @Test
    fun failed_initial_read_keeps_the_today_dashboard_mounted() {
        val state = TodayUiState(
            payload = null,
            loading = false,
            primaryFailure = Failure(
                kind = FailureKind.Unreachable,
                headline = "Can't reach Magician",
                detail = "Start Magician and try again.",
            ),
        )

        assertEquals(TodayContentMode.Dashboard, todayContentMode(state))
    }

    @Test
    fun first_read_spinner_is_the_only_state_that_temporarily_hides_the_dashboard() {
        assertEquals(
            TodayContentMode.Loading,
            todayContentMode(TodayUiState(payload = null, loading = true)),
        )
        assertEquals(
            TodayContentMode.Dashboard,
            todayContentMode(TodayUiState(payload = null, loading = false)),
        )
    }
}
