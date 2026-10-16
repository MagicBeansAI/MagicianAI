import EventKit
import Foundation
import UIKit

struct AppleReminderCreation: Equatable {
    let identifier: String
}

struct PendingAppleReminderReceipt: Codable, Equatable {
    let idempotencyKey: String
    let identifier: String
    let title: String
    let notes: String
    let dueAt: Date
    let timeZoneIdentifier: String
    let createdAt: Date
}

/// A native reminder can outlive its originating sheet or app process before
/// Magician acknowledges the receipt. Persist that small pending handoff so
/// reopening the same card retries the same EventKit identifier and payload.
@MainActor
final class AppleReminderPendingReceiptStore {
    static let shared = AppleReminderPendingReceiptStore()

    private let defaults: UserDefaults
    private let storageKey = "appleReminderPendingReceipts.v1"
    private let maximumReceipts = 64

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    nonisolated static func operationKey(candidateID: String) -> String {
        candidateID
    }

    func receipt(for operationKey: String) -> PendingAppleReminderReceipt? {
        receipts()[operationKey]
    }

    func save(_ receipt: PendingAppleReminderReceipt, for operationKey: String) {
        var current = receipts()
        current[operationKey] = receipt
        if current.count > maximumReceipts {
            for key in current
                .sorted(by: { $0.value.createdAt > $1.value.createdAt })
                .dropFirst(maximumReceipts)
                .map(\.key) {
                current.removeValue(forKey: key)
            }
        }
        persist(current)
    }

    func remove(for operationKey: String) {
        var current = receipts()
        current.removeValue(forKey: operationKey)
        persist(current)
    }

    private func receipts() -> [String: PendingAppleReminderReceipt] {
        guard let data = defaults.data(forKey: storageKey),
              let value = try? JSONDecoder().decode([String: PendingAppleReminderReceipt].self, from: data)
        else { return [:] }
        return value
    }

    private func persist(_ receipts: [String: PendingAppleReminderReceipt]) {
        if receipts.isEmpty {
            defaults.removeObject(forKey: storageKey)
        } else if let data = try? JSONEncoder().encode(receipts) {
            defaults.set(data, forKey: storageKey)
        }
    }
}

enum AppleReminderServiceError: LocalizedError {
    case accessDenied
    case noWritableList

    var errorDescription: String? {
        switch self {
        case .accessDenied:
            return "Allow Magican to access Reminders in Settings, then try again."
        case .noWritableList:
            return "Apple Reminders has no writable default list."
        }
    }
}

/// Owns the EventKit store for the lifetime of the app. EventKit objects must
/// not outlive or cross between stores, so one retained instance is safer than
/// constructing a short-lived store for each card action.
@MainActor
final class AppleReminderService {
    static let shared = AppleReminderService()

    private let eventStore = EKEventStore()
    private let remindersURL = URL(string: "x-apple-reminderkit://")!

    private init() {}

    func create(title: String, notes: String, dueAt: Date, timeZone: TimeZone) async throws -> AppleReminderCreation {
        let granted = try await eventStore.requestFullAccessToReminders()
        guard granted else { throw AppleReminderServiceError.accessDenied }
        guard let list = eventStore.defaultCalendarForNewReminders() else {
            throw AppleReminderServiceError.noWritableList
        }

        let reminder = EKReminder(eventStore: eventStore)
        reminder.title = title.trimmingCharacters(in: .whitespacesAndNewlines)
        reminder.notes = notes.trimmingCharacters(in: .whitespacesAndNewlines)
        reminder.calendar = list
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = timeZone
        reminder.dueDateComponents = calendar.dateComponents(
            [.calendar, .timeZone, .year, .month, .day, .hour, .minute],
            from: dueAt
        )
        reminder.addAlarm(EKAlarm(absoluteDate: dueAt))
        try eventStore.save(reminder, commit: true)

        return AppleReminderCreation(identifier: reminder.calendarItemIdentifier)
    }

    /// Opening the app is deliberately separate from creation. The caller first
    /// records the native identifier with Magician, avoiding suspension between
    /// the EventKit write and the durable resurfacing receipt.
    func openReminders() async -> Bool {
        await withCheckedContinuation { continuation in
            UIApplication.shared.open(remindersURL, options: [:]) { opened in
                continuation.resume(returning: opened)
            }
        }
    }
}
