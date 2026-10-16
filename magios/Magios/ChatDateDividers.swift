import Foundation

/// Day labels for the chat transcript's date dividers and the floating date
/// pill: "Today", "Yesterday", a weekday within the last week, then a medium
/// date. Local calendar throughout. iOS port of
/// `ui/unified-ui/src/lib/magician/chat/dayLabel.ts`.
enum ChatDateDividers {
    /// For timestamps in display order, the indices that open a new local day
    /// (the first dated message always opens one). A message without a time
    /// (nil) never opens a day and does not break the run it sits in.
    static func insertionIndices(for timestamps: [Date?], calendar: Calendar = .current) -> [Int] {
        var indices: [Int] = []
        var previousDay: DateComponents?
        for (index, timestamp) in timestamps.enumerated() {
            guard let timestamp else { continue }
            let day = calendar.dateComponents([.era, .year, .month, .day], from: timestamp)
            if day != previousDay { indices.append(index) }
            previousDay = day
        }
        return indices
    }

    /// "Today", "Yesterday", the weekday for 2–6 days back, else a medium
    /// date ("Sep 24, 2026"). A future day reads as its date.
    static func label(
        for date: Date,
        now: Date = Date(),
        calendar: Calendar = .current,
        locale: Locale = .current
    ) -> String {
        let days = calendar.dateComponents(
            [.day],
            from: calendar.startOfDay(for: date),
            to: calendar.startOfDay(for: now)
        ).day ?? 0
        if days == 0 { return "Today" }
        if days == 1 { return "Yesterday" }
        if days > 1 && days < 7 {
            let formatter = DateFormatter()
            formatter.calendar = calendar
            formatter.timeZone = calendar.timeZone
            formatter.locale = locale
            formatter.setLocalizedDateFormatFromTemplate("EEEE")
            return formatter.string(from: date)
        }
        let formatter = DateFormatter()
        formatter.calendar = calendar
        formatter.timeZone = calendar.timeZone
        formatter.locale = locale
        formatter.dateStyle = .medium
        formatter.timeStyle = .none
        return formatter.string(from: date)
    }

    /// The divider label for each index that opens a day.
    static func labels(
        for timestamps: [Date?],
        now: Date = Date(),
        calendar: Calendar = .current,
        locale: Locale = .current
    ) -> [Int: String] {
        var labels: [Int: String] = [:]
        for index in insertionIndices(for: timestamps, calendar: calendar) {
            if let date = timestamps[index] {
                labels[index] = label(for: date, now: now, calendar: calendar, locale: locale)
            }
        }
        return labels
    }

    /// Epoch milliseconds (the wire's `created_at`), or seconds below 1e11.
    /// Nil for zero, negative or non-finite values.
    static func date(epoch value: Double) -> Date? {
        guard value.isFinite, value > 0 else { return nil }
        return Date(timeIntervalSince1970: value < 1e11 ? value : value / 1_000)
    }

    /// The floating pill's day: the topmost visible dated message's day, or
    /// nil when none is dated or when a visible divider already shows that
    /// day (the pill never doubles a divider). `visibleIndices` in any order.
    static func floatingLabel(
        timestamps: [Date?],
        visibleIndices: Set<Int>,
        now: Date = Date(),
        calendar: Calendar = .current,
        locale: Locale = .current
    ) -> String? {
        guard let top = visibleIndices.sorted().first(where: { $0 < timestamps.count && timestamps[$0] != nil }),
              let date = timestamps[top] else { return nil }
        let topLabel = Self.label(for: date, now: now, calendar: calendar, locale: locale)
        let dividers = labels(for: timestamps, now: now, calendar: calendar, locale: locale)
        let visibleDividers = visibleIndices.compactMap { dividers[$0] }
        return visibleDividers.contains(topLabel) ? nil : topLabel
    }
}
