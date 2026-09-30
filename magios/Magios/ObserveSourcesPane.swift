import SwiftUI

/// Observe → Sources. Two groups:
///  - **This phone** (editable): microphone permission, notifications + Live
///    Activities, and the screen-broadcast note.
///  - **Web & accounts** (view-only, managed on the web): mail & chat channels,
///    calendar observation, continuous sources, browser tabs, startup catch-up.
///    Each block loads, fails and retries on its own.
struct ObserveSourcesPane: View {
    @ObservedObject var sources: ObserveSourcesViewModel
    @ObservedObject var device: ObserveDeviceStatus
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            thisPhone
            VStack(alignment: .leading, spacing: 10) {
                ObserveSectionHeader("Web & accounts")
                Text("Read-only here — change these on the web console.")
                    .font(.caption)
                    .foregroundColor(theme.secondaryTextColor)
                channelsBlock
                calendarBlock
                subscriptionsBlock
                ambientBlock
                catchUpBlock
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("observe-sources")
    }

    // MARK: This phone

    private var thisPhone: some View {
        VStack(alignment: .leading, spacing: 10) {
            ObserveSectionHeader("This phone")
            ObserveCard(spacing: 14) {
                permissionRow(
                    icon: "mic.fill",
                    title: "Microphone",
                    detail: "Listen here and the \u{201C}You\u{201D} track of a screen share use it.",
                    state: device.microphone,
                    request: { Task { await device.requestMicrophone() } }
                )
                Divider()
                permissionRow(
                    icon: "bell.badge.fill",
                    title: "Notifications",
                    detail: device.liveActivitiesEnabled
                        ? "Live Activities on — a live capture shows on the Lock Screen and Dynamic Island."
                        : "Live Activities off — a live capture won't show on the Lock Screen. Turn them on in Settings.",
                    state: device.notifications,
                    request: { Task { await device.requestNotifications() } }
                )
                Divider()
                HStack(alignment: .top, spacing: 12) {
                    rowIcon("rectangle.on.rectangle")
                    VStack(alignment: .leading, spacing: 3) {
                        Text("Screen").font(.subheadline.weight(.semibold)).foregroundColor(theme.textColor)
                        Text("Share screen uses an iOS screen broadcast (Now → Share screen → Prepare session). iOS asks each time; protected call audio (FaceTime / VoIP) is never delivered.")
                            .font(.caption)
                            .foregroundColor(theme.secondaryTextColor)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
            }
        }
    }

    private func rowIcon(_ name: String) -> some View {
        Image(systemName: name)
            .font(.system(size: 13, weight: .semibold))
            .foregroundColor(theme.accentColor)
            .frame(width: 30, height: 30)
            .background(theme.accentColor.opacity(0.12))
            .clipShape(RoundedRectangle(cornerRadius: 8))
    }

    private func permissionRow(
        icon: String,
        title: String,
        detail: String,
        state: ObserveDeviceStatus.Permission,
        request: @escaping () -> Void
    ) -> some View {
        HStack(alignment: .top, spacing: 12) {
            rowIcon(icon)
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    Text(title).font(.subheadline.weight(.semibold)).foregroundColor(theme.textColor)
                    statePill(state)
                }
                Text(detail)
                    .font(.caption)
                    .foregroundColor(theme.secondaryTextColor)
                    .fixedSize(horizontal: false, vertical: true)
                switch state {
                case .notAsked:
                    Button("Allow \(title.lowercased())", action: request)
                        .font(.caption.weight(.semibold))
                        .buttonStyle(.bordered).controlSize(.small)
                        .padding(.top, 2)
                case .denied:
                    Button("Open Settings") { device.openSystemSettings() }
                        .font(.caption.weight(.semibold))
                        .buttonStyle(.bordered).controlSize(.small)
                        .padding(.top, 2)
                case .granted:
                    EmptyView()
                }
            }
            Spacer(minLength: 0)
        }
        .accessibilityElement(children: .contain)
    }

    private func statePill(_ state: ObserveDeviceStatus.Permission) -> some View {
        let color: Color = {
            switch state {
            case .granted: return theme.successColor
            case .denied: return theme.dangerColor
            case .notAsked: return theme.warningColor
            }
        }()
        return Text(state.label)
            .font(.caption2.weight(.bold))
            .foregroundColor(color)
            .padding(.horizontal, 7).padding(.vertical, 2)
            .background(color.opacity(0.13))
            .clipShape(Capsule())
    }

    // MARK: Web & accounts blocks

    private func block<V: Equatable, Content: View>(
        title: String,
        icon: String,
        state: ObserveBlock<V>,
        on: Bool?,
        retry: @escaping () async -> Void,
        @ViewBuilder content: @escaping (V) -> Content
    ) -> some View {
        ObserveCard(spacing: 8) {
            HStack(spacing: 10) {
                Image(systemName: icon)
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundColor(theme.infoColor)
                    .frame(width: 26, height: 26)
                    .background(theme.infoColor.opacity(0.12))
                    .clipShape(RoundedRectangle(cornerRadius: 7))
                Text(title).font(.subheadline.weight(.semibold)).foregroundColor(theme.textColor)
                Spacer(minLength: 4)
                if state.loading { ProgressView().scaleEffect(0.7) }
                if let on { onOffPill(on) }
            }
            if let err = state.error {
                ObserveBanner(text: err, color: theme.dangerColor) { Task { await retry() } }
            }
            if let value = state.value {
                content(value)
            } else if !state.settled {
                Text("Loading…").font(.caption).foregroundColor(theme.secondaryTextColor)
            }
            Text("Manage on web")
                .font(.caption2.weight(.semibold))
                .foregroundColor(theme.secondaryTextColor.opacity(0.8))
        }
    }

    private func onOffPill(_ on: Bool) -> some View {
        Text(on ? "On" : "Off")
            .font(.caption2.weight(.bold))
            .foregroundColor(on ? theme.successColor : theme.secondaryTextColor)
            .padding(.horizontal, 7).padding(.vertical, 2)
            .background((on ? theme.successColor : theme.secondaryTextColor).opacity(0.13))
            .clipShape(Capsule())
    }

    private func emptyLine(_ text: String) -> some View {
        Text(text).font(.caption).foregroundColor(theme.secondaryTextColor)
    }

    private var channelsBlock: some View {
        block(
            title: "Mail & chat channels",
            icon: "envelope.fill",
            state: sources.channels,
            on: sources.channels.value.map { $0.contains(where: \.enabled) },
            retry: { await sources.reloadChannels() }
        ) { channels in
            if channels.isEmpty {
                emptyLine("No mail or chat accounts connected.")
            } else {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(channels) { c in
                        VStack(alignment: .leading, spacing: 2) {
                            HStack(spacing: 6) {
                                Text("\(c.providerLabel) · \(c.account)")
                                    .font(.caption.weight(.semibold))
                                    .foregroundColor(theme.textColor)
                                    .lineLimit(1)
                                if c.verificationCodes {
                                    Text("Codes")
                                        .font(.system(size: 9, weight: .bold))
                                        .foregroundColor(theme.discoveryColor)
                                        .padding(.horizontal, 5).padding(.vertical, 1)
                                        .background(theme.discoveryColor.opacity(0.13))
                                        .clipShape(Capsule())
                                        .accessibilityLabel("Verification codes granted")
                                }
                                Spacer(minLength: 4)
                                onOffPill(c.enabled)
                            }
                            Text([
                                c.connected ? "Connected" : "Not connected",
                                "\(c.messageCount) synced",
                                c.lane.map { "\(ObserveChannel.titleCase($0)) lane" } ?? "",
                            ].filter { !$0.isEmpty }.joined(separator: " · "))
                                .font(.caption2)
                                .foregroundColor(theme.secondaryTextColor)
                        }
                    }
                }
            }
        }
    }

    private var calendarBlock: some View {
        block(
            title: "Calendar observation",
            icon: "calendar",
            state: sources.calendar,
            on: sources.calendar.value?.enabled,
            retry: { await sources.reloadCalendar() }
        ) { cal in
            VStack(alignment: .leading, spacing: 3) {
                Text(cal.accounts.isEmpty ? "No accounts selected" : cal.accounts.joined(separator: ", "))
                    .font(.caption)
                    .foregroundColor(theme.textColor)
                    .lineLimit(2)
                Text([
                    cal.cadenceText ?? "",
                    "Last sync \(ObserveTime.relative(cal.lastSyncAt))",
                ].filter { !$0.isEmpty }.joined(separator: " · "))
                    .font(.caption2)
                    .foregroundColor(theme.secondaryTextColor)
            }
        }
    }

    private var subscriptionsBlock: some View {
        block(
            title: "Continuous sources",
            icon: "dot.radiowaves.left.and.right",
            state: sources.subscriptions,
            on: nil,
            retry: { await sources.reloadSubscriptions() }
        ) { subs in
            if subs.isEmpty {
                emptyLine("No continuous sources are listening.")
            } else {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(subs) { s in
                        VStack(alignment: .leading, spacing: 2) {
                            HStack(spacing: 6) {
                                Text(s.name)
                                    .font(.caption.weight(.semibold))
                                    .foregroundColor(theme.textColor)
                                    .lineLimit(1)
                                Spacer(minLength: 4)
                                Text(s.stateLabel)
                                    .font(.caption2.weight(.bold))
                                    .foregroundColor(s.stateLabel == "Listening" ? theme.successColor : theme.warningColor)
                            }
                            Text("Via \(s.provider) · Last check \(ObserveTime.relative(s.lastSuccessAt)) · Next \(ObserveTime.relative(s.nextRunAt))")
                                .font(.caption2)
                                .foregroundColor(theme.secondaryTextColor)
                                .lineLimit(2)
                        }
                    }
                    if let total = sources.enabledSubscriptionTotal, total > subs.count {
                        emptyLine("+\(total - subs.count) more on the web")
                    }
                }
            }
        }
    }

    private var ambientBlock: some View {
        block(
            title: "Browser tabs",
            icon: "globe",
            state: sources.ambient,
            on: sources.ambient.value?.enabled,
            retry: { await sources.reloadAmbient() }
        ) { a in
            Text([
                a.paired ? "Extension paired" : "No browser paired",
                "\(a.acceptedToday ?? a.totalSignals) signals\(a.acceptedToday != nil ? " today" : "")",
                "last capture \(ObserveTime.relative(a.lastSignalAt))",
            ].joined(separator: " · "))
                .font(.caption)
                .foregroundColor(theme.secondaryTextColor)
        }
    }

    private var catchUpBlock: some View {
        block(
            title: "Startup catch-up",
            icon: "arrow.counterclockwise",
            state: sources.catchUp,
            on: sources.catchUp.value?.policyEnabled,
            retry: { await sources.reloadCatchUp() }
        ) { c in
            VStack(alignment: .leading, spacing: 2) {
                Text(c.phaseLabel)
                    .font(.caption.weight(.semibold))
                    .foregroundColor(theme.textColor)
                Text(c.progressLine)
                    .font(.caption2)
                    .foregroundColor(theme.secondaryTextColor)
            }
        }
    }
}
