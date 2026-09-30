//  MonitorsViewModel.swift
//  Recurring Monitors (Phase 5, iOS) — list + detail view models.
//
//  The list is CURSOR-paginated (plan §8): accumulate-and-load-more with a
//  generation guard, mirroring the web `src/lib/monitors/pagination.ts` —
//  concurrent load-mores coalesce, filter switches invalidate late responses,
//  and rows dedupe by `task_id` across page boundaries. The envelope's `total`
//  rides beside the cursor and says how big the pool is; the cursor keeps its
//  exact meaning and is still what walks it. The detail loads the
//  four §5.3 sections (Latest / Updates / Runs / Settings) and dispatches the
//  lifecycle actions through `Monitors.APIClient`.

import Foundation
import SwiftUI

extension Monitors {

    /// Production client wired to the app's single scope source of truth
    /// (`MagicianAccess` — anonymous/default; NEVER a hardcoded other scope).
    static func liveClient(session: URLSession = .shared) -> APIClient {
        APIClient(
            baseURL: MagicianAccess.baseURL,
            scope: Scope(principal: MagicianAccess.principal,
                         workspace: MagicianAccess.workspace),
            transport: URLSessionTransport(session: session,
                                           extraHeaders: MagicianAccess.authorizedHeaders(
                                               for: MagicianAccess.baseURL
                                           )))
    }
}

// MARK: - List

@MainActor
final class MonitorsListViewModel: ObservableObject {

    /// Server-side `state` filter chips (`active`/`paused` ride the query).
    enum StateFilter: String, CaseIterable, Identifiable {
        case all, active, paused
        var id: String { rawValue }
        var title: String {
            switch self {
            case .all: return "All"
            case .active: return "Active"
            case .paused: return "Paused"
            }
        }
        /// The `state` query value (nil = no filter).
        var queryValue: String? { self == .all ? nil : rawValue }
    }

    @Published private(set) var items: [Monitors.ListItemV1] = []
    @Published private(set) var nextCursor: String?
    /// Monitors in the whole filtered pool, as the envelope reports it beside
    /// the cursor — the one thing a cursor cannot express.
    ///
    /// `nil` until a response carries one, and `nil` is a real state: a binary
    /// that reports no total never counted the corpus, so the list can offer
    /// "load more" but cannot say how much more. Reading that as zero would
    /// print an empty pool over rows that are on the screen.
    @Published private(set) var total: Int?
    /// First-page load in flight (initial, filter switch, or retry).
    @Published private(set) var isLoading = false
    @Published private(set) var isLoadingMore = false
    @Published private(set) var errorMessage: String?
    /// At least one load finished (distinguishes empty from never-loaded).
    @Published private(set) var hasLoadedOnce = false
    @Published private(set) var stateFilter: StateFilter = .all

    let pageSize: Int
    /// The widest `limit` `/monitors` will honour (it clamps to 200). A span
    /// refresh asks for what the reader holds, capped here; past the cap the
    /// list is trimmed rather than lost, and the response's own cursor still
    /// resumes exactly where the trimmed list ends.
    static let maxRequestPageSize = 200
    /// Read-only exposure (not private) so `TasksView`'s `magican://task/{id}`
    /// monitor probe rides the SAME injectable client as the lane list —
    /// tests inject a mock here instead of the probe spinning up an
    /// ephemeral live client.
    let client: Monitors.APIClient
    /// Invalidates in-flight responses on filter switches/reloads.
    private var generation = 0
    /// Rows the reader has asked for — one page, plus one per Load more, reset
    /// to one page by a filter switch. Kept APART from `items.count` so a row
    /// a delete just dropped does not shrink the window the delete's own
    /// refresh asks for: the reader paged for those rows and still has them.
    ///
    /// A window size, never a position — the cursor is what walks the corpus,
    /// so asking for more than exists is answered with everything there is.
    private var loadedSpan: Int

    init(client: Monitors.APIClient? = nil, pageSize: Int = 50) {
        self.client = client ?? Monitors.liveClient()
        self.pageSize = pageSize
        self.loadedSpan = pageSize
    }

    /// Load the first page if nothing is loaded yet (lane appear).
    func loadIfNeeded() async {
        guard !hasLoadedOnce, !isLoading else { return }
        await reload()
    }

    /// Replace the list with a fresh first page (pull-to-refresh / retry) —
    /// a deliberate restart, where page one IS the answer asked for.
    func reload() async {
        await load(limit: pageSize)
    }

    /// Re-read the rows the reader is HOLDING, from the top, and take the
    /// cursor from the answer that carried them.
    ///
    /// This is the refresh a mutation asks for. A delete is not a restart: the
    /// reader paged four times to reach that monitor and must still have those
    /// pages once it is gone.
    ///
    /// Re-priming the cursor is not optional here. `next_cursor` is minted
    /// from the LAST row of the page that carried it, and `/monitors` resolves
    /// a cursor it can no longer find to the end of the list — so dropping the
    /// last-loaded monitor and keeping its cursor would terminate pagination
    /// with no visible sign. Asking the server for the span returns a cursor
    /// anchored on a row that still exists.
    func refreshLoadedSpan() async {
        await load(limit: min(max(pageSize, loadedSpan), Self.maxRequestPageSize))
    }

    private func load(limit: Int) async {
        generation += 1
        let thisGeneration = generation
        isLoading = true
        // A reload PREEMPTS any in-flight load-more (the generation bump
        // discards its completion), so clear the flag UP-FRONT, before the
        // first await — a preempted load-more whose trailing reset loses the
        // generation race must not leave `isLoadingMore` stuck true.
        isLoadingMore = false
        errorMessage = nil
        do {
            let page = try await client.list(limit: limit, cursor: nil,
                                             state: stateFilter.queryValue)
            guard thisGeneration == generation else { return }
            items = Self.dedupe(page.items)
            nextCursor = page.nextCursor
            // Straight from the page, absence included: the answer that
            // carried these rows is the only one entitled to count them.
            total = page.total
            loadedSpan = max(pageSize, limit)
            hasLoadedOnce = true
        } catch {
            guard thisGeneration == generation else { return }
            errorMessage = Self.message(for: error)
        }
        if thisGeneration == generation {
            isLoading = false
            isLoadingMore = false
        }
    }

    /// Fetch the next cursor page and append (deduped). Concurrent calls
    /// coalesce onto the in-flight request.
    func loadMore() async {
        guard let cursor = nextCursor, !isLoadingMore, !isLoading else { return }
        generation += 1
        let thisGeneration = generation
        isLoadingMore = true
        do {
            let page = try await client.list(limit: pageSize, cursor: cursor,
                                             state: stateFilter.queryValue)
            guard thisGeneration == generation else { return }
            items = Self.dedupe(items + page.items)
            nextCursor = page.nextCursor
            // Every page reports the SAME corpus size (the server counts the
            // pool, not the remainder), so taking the newest is a refresh of
            // one number rather than a running subtraction.
            total = page.total
            loadedSpan = max(loadedSpan, items.count)
        } catch {
            guard thisGeneration == generation else { return }
            // Keep accumulated rows; surface the page error for Retry.
            errorMessage = Self.message(for: error)
        }
        if thisGeneration == generation { isLoadingMore = false }
    }

    /// Switch the server-side state filter and reload from page one.
    func setFilter(_ filter: StateFilter) async {
        guard filter != stateFilter else { return }
        stateFilter = filter
        items = []
        nextCursor = nil
        // Each filter is a different corpus, so the count goes with the rows
        // it counted — a stale total would label the next lane's page.
        total = nil
        await reload()
    }

    /// Reflect a pause/resume response into the row without a refetch.
    func applyState(taskID: String, state: String) {
        guard let index = items.firstIndex(where: { $0.taskID == taskID }) else { return }
        items[index].state = state
        if let filtered = stateFilter.queryValue, filtered != state {
            items.remove(at: index)
            dropFromTotal(1)
        }
    }

    /// Drop a deleted monitor's row.
    ///
    /// The row and the count move at once so the footer never quotes a total
    /// over a list that no longer matches it. The CURSOR is deliberately left
    /// alone here: the deleted row may be the one it was minted from, and this
    /// method cannot tell — `refreshLoadedSpan()` is what re-primes it, and
    /// every caller of this runs it.
    func remove(taskID: String) {
        let before = items.count
        items.removeAll { $0.taskID == taskID }
        dropFromTotal(before - items.count)
    }

    /// A row this client dropped left the server's pool too, so the count
    /// beside the rows has to follow it down.
    ///
    /// Monitors has no completion grace period — the Tasks list holds a
    /// just-ticked row for 5s because the server drops it from the lane at
    /// once, but nothing here is time-held, and pause/delete are echoed
    /// straight into `items`. That is what makes this necessary: neither path
    /// refetches, so without it the footer keeps quoting the total the last
    /// page carried and reads "4 of 5" over four rows forever.
    private func dropFromTotal(_ removed: Int) {
        guard removed > 0, let current = total else { return }
        total = max(0, current - removed)
    }

    private static func dedupe(_ rows: [Monitors.ListItemV1]) -> [Monitors.ListItemV1] {
        var seen = Set<String>()
        return rows.filter { seen.insert($0.taskID).inserted }
    }

    static func message(for error: Error) -> String {
        if let apiError = error as? Monitors.APIError { return apiError.userMessage }
        return error.localizedDescription
    }
}

// MARK: - Detail

@MainActor
final class MonitorDetailViewModel: ObservableObject {

    enum MonitorAction: String, Equatable {
        case run, pause, resume, delete
    }

    let taskID: String
    /// The exact update record a deep link targets (highlighted in Updates).
    let highlightUpdateID: String?

    @Published private(set) var detail: Monitors.DetailV1?
    @Published private(set) var updates: [Monitors.UpdateDetailV1] = []
    @Published private(set) var runs: [Monitors.RunResultV1] = []
    /// Per-update verdict state (stored + optimistic), keyed by update id
    /// (Phase 6, plan §10).
    @Published private(set) var feedbackByUpdate: [String: Monitors.UpdateFeedbackState] = [:]
    @Published private(set) var isLoading = false
    @Published private(set) var loadErrorMessage: String?
    @Published private(set) var busyAction: MonitorAction?
    @Published var actionErrorMessage: String?
    @Published private(set) var actionNoticeMessage: String?
    /// Set after a successful delete so the presenting view dismisses.
    @Published private(set) var wasDeleted = false

    private let client: Monitors.APIClient
    private var generation = 0

    init(taskID: String, highlightUpdateID: String? = nil,
         client: Monitors.APIClient? = nil) {
        self.taskID = taskID
        self.highlightUpdateID = highlightUpdateID
        self.client = client ?? Monitors.liveClient()
    }

    /// The newest update record — the detail's "Latest" section.
    var latestUpdate: Monitors.UpdateDetailV1? { updates.first }

    var isPaused: Bool { detail.map { paused($0) } ?? false }

    private func paused(_ detail: Monitors.DetailV1) -> Bool {
        detail.schedule?.paused == true
    }

    var cadenceSummary: String { Monitors.cadenceSummary(detail?.schedule) }

    /// Load detail + updates + runs + stored feedback. Detail failure is the
    /// load error; updates/runs/feedback failures degrade to empty sections
    /// (partial tolerance — the settings/actions surface stays usable
    /// offline history or not).
    func load() async {
        generation += 1
        let thisGeneration = generation
        isLoading = true
        loadErrorMessage = nil
        do {
            async let detailFetch = client.detail(taskID)
            async let updatesFetch = client.updates(taskID)
            async let runsFetch = client.runs(taskID)
            async let feedbackFetch = client.feedback(taskID)
            let fetched = try await detailFetch
            guard thisGeneration == generation else { return }
            detail = fetched
            updates = (try? await updatesFetch)?.items ?? []
            runs = (try? await runsFetch)?.items ?? []
            feedbackByUpdate = Monitors.feedbackStates(
                from: (try? await feedbackFetch)?.items ?? [])
        } catch {
            guard thisGeneration == generation else { return }
            loadErrorMessage = MonitorsListViewModel.message(for: error)
        }
        if thisGeneration == generation { isLoading = false }
    }

    // MARK: - Feedback (Phase 6, plan §10)

    /// The stored/optimistic verdict for one update record, if any.
    func verdict(for updateID: String) -> Monitors.FeedbackVerdict? {
        feedbackByUpdate[updateID]?.verdict
    }

    /// True while this update's verdict POST is in flight (chips disable).
    func isFeedbackInFlight(_ updateID: String) -> Bool {
        feedbackByUpdate[updateID]?.inFlight == true
    }

    /// Useful / Not relevant on one material update. Optimistic: the tapped
    /// verdict shows immediately, settles from the POST response (identical
    /// for the idempotent `recorded:false` replay — it carries the same
    /// authoritative verdict + feedback id), and rolls back into
    /// `actionErrorMessage` on error. Per-update serialization only —
    /// feedback does not lock the lifecycle action row.
    @discardableResult
    func submitFeedback(updateID: String,
                        verdict: Monitors.FeedbackVerdict) async -> Bool {
        guard feedbackByUpdate[updateID]?.inFlight != true else { return false }
        let previous = feedbackByUpdate[updateID]
        feedbackByUpdate[updateID] = Monitors.UpdateFeedbackState(
            verdict: verdict, feedbackID: nil, inFlight: true)
        actionErrorMessage = nil
        do {
            let response = try await client.submitFeedback(
                taskID, updateID: updateID, verdict: verdict)
            feedbackByUpdate[updateID] = Monitors.UpdateFeedbackState(
                verdict: response.verdict, feedbackID: response.feedbackID,
                inFlight: false)
            return true
        } catch {
            // Roll the optimistic verdict back to what was stored before.
            if var previous {
                previous.inFlight = false
                feedbackByUpdate[updateID] = previous
            } else {
                feedbackByUpdate[updateID] = nil
            }
            actionErrorMessage = MonitorsListViewModel.message(for: error)
            return false
        }
    }

    // MARK: - Actions

    /// Run-now through the exact task execute path. Returns success.
    @discardableResult
    func runNow() async -> Bool {
        await perform(.run, notice: "Run started.") { [client, taskID] in
            try await client.runNow(taskID)
            return nil
        }
    }

    @discardableResult
    func pause() async -> Bool {
        await perform(.pause, notice: "Monitor paused.") { [client, taskID] in
            try await client.pause(taskID).state
        }
    }

    @discardableResult
    func resume() async -> Bool {
        await perform(.resume, notice: "Monitor resumed.") { [client, taskID] in
            try await client.resume(taskID).state
        }
    }

    /// Soft delete (server archives the task and stamps it gone from every
    /// monitor surface). The VIEW confirms before calling.
    @discardableResult
    func delete() async -> Bool {
        let ok = await perform(.delete, notice: nil) { [client, taskID] in
            _ = try await client.delete(taskID)
            return nil
        }
        if ok { wasDeleted = true }
        return ok
    }

    /// One serialized action at a time; pause/resume fold the returned state
    /// back into the loaded schedule so the UI flips without a refetch.
    private func perform(
        _ action: MonitorAction, notice: String?,
        _ body: @escaping () async throws -> String?
    ) async -> Bool {
        guard busyAction == nil else { return false }
        busyAction = action
        actionErrorMessage = nil
        actionNoticeMessage = nil
        defer { busyAction = nil }
        do {
            let newState = try await body()
            if let newState, var current = detail, var schedule = current.schedule {
                schedule.paused = (newState == "paused")
                current.schedule = schedule
                detail = current
            }
            actionNoticeMessage = notice
            return true
        } catch {
            actionErrorMessage = MonitorsListViewModel.message(for: error)
            return false
        }
    }
}

// MARK: - Composer (create/edit submission)

@MainActor
final class MonitorComposerViewModel: ObservableObject {

    enum Mode: Equatable {
        case create
        /// Edit an existing monitor (PATCH replaces title/spec/schedule).
        case edit(taskID: String)
        /// Phase 7: convert an EXISTING eligible task into a monitor.
        /// POSTs `{spec, title?}` to `/monitors/{id}/convert` — never a
        /// schedule (the task keeps its own; `keptCadence` is the human
        /// summary the sheet shows, e.g. "Cron 0 9 * * *" or "unscheduled").
        case convert(taskID: String, keptCadence: String)
    }

    /// The two-step flow: edit fields → review the normalized contract +
    /// exact cadence → activate (the POST/PATCH happens ONLY from review).
    enum Step: Equatable {
        case form
        case review(spec: Monitors.SpecV1, schedule: Monitors.ScheduleWire?)
    }

    let mode: Mode
    @Published var form: MonitorForm
    @Published private(set) var step: Step = .form
    @Published private(set) var isSubmitting = false
    @Published var errorMessage: String?

    private let client: Monitors.APIClient

    init(mode: Mode, form: MonitorForm = MonitorForm(),
         client: Monitors.APIClient? = nil) {
        self.mode = mode
        self.form = form
        self.client = client ?? Monitors.liveClient()
    }

    var isReviewing: Bool {
        if case .review = step { return true }
        return false
    }

    /// Convert mode never authors a schedule — the task keeps its own.
    var isConvert: Bool {
        if case .convert = mode { return true }
        return false
    }

    /// Validate through the admission mirror and advance to review.
    /// Returns false (with the stable reason's label) on rejection.
    @discardableResult
    func review() -> Bool {
        errorMessage = nil
        let spec: Monitors.SpecV1
        switch MonitorForm.validateAndNormalize(form.buildSpec()) {
        case .ok(let normalized): spec = normalized
        case .rejected(let reason):
            errorMessage = MonitorForm.reasonLabel(reason)
            return false
        }
        let schedule: Monitors.ScheduleWire?
        if isConvert {
            // Conversion carries no schedule on the wire; the review step
            // shows the Mode's keptCadence instead.
            schedule = nil
        } else {
            switch form.buildSchedule() {
            case .ok(let built): schedule = built
            case .rejected(let reason):
                errorMessage = MonitorForm.reasonLabel(reason)
                return false
            }
        }
        step = .review(spec: spec, schedule: schedule)
        return true
    }

    func backToForm() {
        step = .form
        errorMessage = nil
    }

    /// POST/PATCH the reviewed contract. In-flight lock prevents a double
    /// submit. Returns success (the view dismisses + refreshes on true).
    @discardableResult
    func activate() async -> Bool {
        guard case .review(let spec, let schedule) = step, !isSubmitting else {
            return false
        }
        isSubmitting = true
        errorMessage = nil
        defer { isSubmitting = false }
        do {
            switch mode {
            case .create:
                _ = try await client.create(title: form.normalizedTitle,
                                            spec: spec, schedule: schedule)
            case .edit(let taskID):
                _ = try await client.update(taskID, title: form.normalizedTitle,
                                            spec: spec, schedule: schedule)
            case .convert(let taskID, _):
                // Phase 7 contract: `{spec, title?}` only — no schedule key.
                _ = try await client.convert(taskID, spec: spec,
                                             title: form.normalizedTitle)
            }
            return true
        } catch {
            errorMessage = MonitorsListViewModel.message(for: error)
            return false
        }
    }
}
