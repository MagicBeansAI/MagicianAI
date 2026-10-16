import Foundation

/// The replace-in-place data-integrity core. Given what the keyboard typed
/// (`shadow`) and a `draft` replacement, it plans a safe edit against the LIVE
/// text before the cursor — and its inverse for Undo. Pure (app-host testable).
///
/// Pre-flight: the live `documentContextBeforeInput` must end with exactly the
/// shadow text, else the field diverged (autocorrect, smart quotes, a webview
/// that doesn't round-trip, a moved cursor) and we must NOT blind-delete — we
/// fall back to insert-only.
public enum KeyboardEditPlan: Equatable {
    /// Delete `deleteCount` graphemes, then insert `insert`. `undo` reverses it.
    case replace(deleteCount: Int, insert: String, undoDeleteCount: Int, undoInsert: String)
    /// Pre-flight failed — append the draft without deleting anything.
    case insertOnly(String)

    public var isReplace: Bool {
        if case .replace = self { return true }
        return false
    }
}

public enum KeyboardEditPlanner {
    /// Plan replacing `shadow` with `draft`, verified against the live text.
    public static func plan(shadow: String, draft: String, documentBefore: String?) -> KeyboardEditPlan {
        let before = documentBefore ?? ""
        if !shadow.isEmpty, before.hasSuffix(shadow) {
            return .replace(
                deleteCount: shadow.count,
                insert: draft,
                undoDeleteCount: draft.count,
                undoInsert: shadow
            )
        }
        return .insertOnly(draft)
    }

    /// Post-verify after applying a `.replace`: the live text must now end with
    /// the inserted draft (no residue). If it doesn't, the caller restores by
    /// re-inserting the original shadow text.
    public static func replaceSucceeded(draft: String, documentBefore: String?) -> Bool {
        (documentBefore ?? "").hasSuffix(draft)
    }

    /// Whether an Undo chip is still valid: the live text must still end with the
    /// draft we inserted (else the user has typed since, and Undo would corrupt).
    public static func undoIsValid(insertedDraft: String, documentBefore: String?) -> Bool {
        !insertedDraft.isEmpty && (documentBefore ?? "").hasSuffix(insertedDraft)
    }
}
