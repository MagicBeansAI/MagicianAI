//  MonitorsListView.swift
//  Recurring Monitors (Phase 5, iOS) — the Monitors lane inside the Tasks
//  surface (§9.3.1): server-paginated rows with state badge, health,
//  VERBATIM `cadence_summary`, and last-run status; active/paused filter
//  chips riding the server `state` query; pull-to-refresh (preserves the
//  navigation title — §9.3.5), load-more on cursor counted against the
//  envelope's `total`, and explicit loading / empty / error+retry / offline
//  states.

import SwiftUI

struct MonitorsLaneView: View {
    @ObservedObject var vm: MonitorsListViewModel
    @ObservedObject private var themeManager = ThemeManager.shared

    let onSelect: (Monitors.ListItemV1) -> Void
    let onCreate: () -> Void

    var body: some View {
        VStack(spacing: 0) {
            filterChips
            content
        }
        .task { await vm.loadIfNeeded() }
    }

    private var filterChips: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                ForEach(MonitorsListViewModel.StateFilter.allCases) { filter in
                    chip(filter.title, active: vm.stateFilter == filter) {
                        Task { await vm.setFilter(filter) }
                    }
                }
            }
            .padding(.horizontal)
        }
        .padding(.vertical, 8)
    }

    @ViewBuilder private var content: some View {
        if vm.isLoading && vm.items.isEmpty {
            loadingState
        } else if let error = vm.errorMessage, vm.items.isEmpty {
            errorState(error)
        } else if vm.items.isEmpty && vm.hasLoadedOnce {
            emptyState
        } else {
            list
        }
    }

    /// Fixed-size placeholder rows during the first load (no empty-state flash).
    private var loadingState: some View {
        ScrollView {
            LazyVStack(spacing: 10) {
                ForEach(0..<4, id: \.self) { _ in
                    RoundedRectangle(cornerRadius: 14)
                        .fill(themeManager.surfaceColor)
                        .frame(height: 96)
                        .overlay(alignment: .center) {
                            ProgressView().controlSize(.small)
                        }
                }
            }
            .padding(.horizontal)
            .padding(.vertical, 10)
        }
        .accessibilityIdentifier("monitors-loading")
    }

    private func errorState(_ message: String) -> some View {
        VStack(spacing: 10) {
            Spacer()
            Image(systemName: "wifi.exclamationmark")
                .font(.system(size: 30))
                .foregroundColor(themeManager.warningColor)
            Text("Monitors could not load")
                .font(.themed(15, weight: .semibold))
                .foregroundColor(themeManager.textColor)
            Text(message)
                .font(.themed(12))
                .foregroundColor(themeManager.secondaryTextColor)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 24)
            Button {
                Task { await vm.reload() }
            } label: {
                Text("Retry")
                    .font(.themed(13, weight: .bold))
                    .padding(.horizontal, 18).padding(.vertical, 8)
                    .background(themeManager.accentColor)
                    .foregroundColor(themeManager.onAccentColor)
                    .clipShape(Capsule())
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("monitors-retry")
            Spacer()
        }
        .frame(maxWidth: .infinity)
    }

    private var emptyState: some View {
        VStack(spacing: 8) {
            Spacer()
            Image(systemName: "binoculars")
                .font(.system(size: 34))
                .foregroundColor(themeManager.secondaryTextColor.opacity(0.5))
            Text(vm.stateFilter == .all ? "No monitors yet" : "No \(vm.stateFilter.rawValue) monitors")
                .font(.themed(15, weight: .semibold))
                .foregroundColor(themeManager.textColor)
            Text("A monitor watches pages you care about and reports material changes.")
                .font(.themed(12))
                .foregroundColor(themeManager.secondaryTextColor)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 32)
            if vm.stateFilter == .all {
                Button(action: onCreate) {
                    Label("Create monitor", systemImage: "plus")
                        .font(.themed(13, weight: .bold))
                        .padding(.horizontal, 16).padding(.vertical, 8)
                        .background(themeManager.accentColor)
                        .foregroundColor(themeManager.onAccentColor)
                        .clipShape(Capsule())
                }
                .buttonStyle(.plain)
                .padding(.top, 4)
                .accessibilityIdentifier("monitors-empty-create")
            }
            Spacer()
        }
        .frame(maxWidth: .infinity)
    }

    private var list: some View {
        ScrollView {
            LazyVStack(spacing: 10) {
                if let error = vm.errorMessage {
                    inlineErrorBanner(error)
                }
                ForEach(vm.items) { item in
                    row(item)
                        .onAppear {
                            // Auto-page when the LAST row becomes visible.
                            if item.taskID == vm.items.last?.taskID {
                                Task { await vm.loadMore() }
                            }
                        }
                }
                if vm.nextCursor != nil {
                    loadMoreFooter
                }
            }
            .padding(.horizontal)
            .padding(.vertical, 10)
        }
        // Pull-to-refresh with a visible spinner; the navigation title is
        // untouched (§9.3.5 — no title swap while refreshing).
        .refreshable { await vm.reload() }
    }

    private func inlineErrorBanner(_ message: String) -> some View {
        HStack(spacing: 8) {
            Image(systemName: "exclamationmark.triangle.fill")
            // Phase 6 a11y: no clamp — a truncated error hides the reason
            // under larger Dynamic Type sizes.
            Text(message).lineLimit(nil)
            Spacer(minLength: 4)
            Button("Retry") { Task { await vm.reload() } }
                .font(.themed(12, weight: .bold))
        }
        .font(.themed(12))
        .foregroundColor(themeManager.warningColor)
        .padding(10)
        .background(themeManager.warningColor.opacity(0.12))
        .clipShape(RoundedRectangle(cornerRadius: 10))
        .accessibilityIdentifier("monitors-error-banner")
    }

    private var loadMoreFooter: some View {
        Button {
            Task { await vm.loadMore() }
        } label: {
            HStack(spacing: 6) {
                if vm.isLoadingMore { ProgressView().controlSize(.small) }
                Text(vm.isLoadingMore ? "Loading…" : loadMoreLabel)
            }
            .font(.themed(13, weight: .semibold))
            .frame(maxWidth: .infinity)
            .padding(.vertical, 10)
            .background(themeManager.surfaceColor)
            .foregroundColor(themeManager.accentColor)
            .clipShape(RoundedRectangle(cornerRadius: 10))
        }
        .buttonStyle(.plain)
        .disabled(vm.isLoadingMore)
        .accessibilityLabel(accessibleLoadMoreLabel)
        .accessibilityIdentifier("monitors-load-more")
    }

    /// How much of the pool is loaded — the Tasks lane's footer idiom, now
    /// that the envelope carries a `total`. Without one the button stays a
    /// plain "Load more": a server that reported no total counted nothing, and
    /// "N of 0" would read as an empty pool under a screen full of rows.
    private var loadMoreLabel: String {
        guard let total = vm.total else { return "Load more" }
        return "Load more · \(vm.items.count) of \(total)"
    }

    private var accessibleLoadMoreLabel: String {
        guard let total = vm.total else { return "Load more monitors" }
        return "Load more monitors, \(vm.items.count) of \(total) loaded"
    }

    // MARK: - Row

    private func row(_ item: Monitors.ListItemV1) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .top, spacing: 8) {
                Text(item.title.isEmpty ? "Untitled monitor" : item.title)
                    .font(.themed(16, weight: .semibold))
                    .foregroundColor(themeManager.textColor)
                    .lineLimit(2)
                Spacer(minLength: 8)
                stateBadge(item.state)
            }
            if !item.objective.isEmpty {
                Text(item.objective)
                    .font(.themed(13))
                    .foregroundColor(themeManager.secondaryTextColor)
                    .lineLimit(2)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            HStack(spacing: 6) {
                // The server-rendered cadence string, shown VERBATIM.
                pill(item.cadenceSummary, tint: themeManager.infoColor)
                if item.health != "ok" {
                    pill(Monitors.healthLabel(item.health), tint: themeManager.warningColor)
                }
                Spacer(minLength: 0)
            }
            HStack(spacing: 6) {
                pill(Monitors.runStatusLabel(item.lastRunStatus),
                     tint: lastRunTint(item.lastRunStatus))
                if let rel = relativeTime(item.lastRunAt) {
                    Text("· \(rel)")
                        .font(.themed(11))
                        .foregroundColor(themeManager.secondaryTextColor)
                }
                Spacer(minLength: 0)
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(themeManager.cardColor)
        .clipShape(RoundedRectangle(cornerRadius: 14))
        .overlay {
            RoundedRectangle(cornerRadius: 14)
                .stroke(themeManager.cardBorderColor, lineWidth: 1)
        }
        .contentShape(Rectangle())
        .onTapGesture { onSelect(item) }
        // Phase 6 a11y: the row is a tap target on a plain VStack — expose
        // it to VoiceOver as ONE button (combined children) so it is
        // announced and activatable, not read as loose text fragments.
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isButton)
        .accessibilityHint("Opens the monitor detail.")
        .accessibilityIdentifier("monitor-row-\(item.taskID)")
    }

    private func stateBadge(_ state: String) -> some View {
        let active = state == "active"
        let tint = active ? themeManager.successColor : themeManager.warningColor
        return Text(active ? "Active" : "Paused")
            .font(.themed(11, weight: .bold))
            .padding(.horizontal, 8).padding(.vertical, 3)
            .background(tint.opacity(0.15))
            .foregroundColor(tint)
            .clipShape(Capsule())
    }

    private func pill(_ label: String, tint: Color) -> some View {
        Text(label)
            .font(.themed(11, weight: .medium))
            .padding(.horizontal, 8).padding(.vertical, 3)
            .background(tint.opacity(0.12))
            .foregroundColor(tint)
            .clipShape(Capsule())
    }

    private func lastRunTint(_ status: String) -> Color {
        switch status {
        case "changed": return themeManager.infoColor
        case "unchanged": return themeManager.successColor
        case "degraded": return themeManager.warningColor
        case "failed": return themeManager.dangerColor
        default: return themeManager.secondaryTextColor // baseline / never_ran
        }
    }

    private func relativeTime(_ iso: String?) -> String? {
        guard let iso, let date = TaskV3.parseISO(iso) else { return nil }
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .short
        return formatter.localizedString(for: date, relativeTo: Date())
    }

    private func chip(_ label: String, active: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Text(label)
                .font(.themed(13, weight: .medium))
                .padding(.horizontal, 12).padding(.vertical, 6)
                .background(active ? themeManager.accentColor : themeManager.surfaceColor)
                .foregroundColor(active ? themeManager.onAccentColor : themeManager.textColor)
                .clipShape(Capsule())
                .overlay(Capsule().stroke(
                    themeManager.secondaryTextColor.opacity(active ? 0 : 0.2), lineWidth: 1))
        }
        .buttonStyle(.plain)
        // Phase 6 a11y: announce which state filter is currently applied.
        .accessibilityAddTraits(active ? .isSelected : [])
        .accessibilityHint("Filters the monitor list.")
    }
}
