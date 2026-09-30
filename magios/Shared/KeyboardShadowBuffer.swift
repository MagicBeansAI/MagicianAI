import Foundation

/// Tracks exactly what the Magican keyboard has typed into the current field since
/// the last reset — the "keyhole" the Write lane rewrites in place. Pure value
/// type (app-host testable). Reset when the field or selection changes.
public struct KeyboardShadowBuffer: Equatable {
    public private(set) var text: String = ""

    public init() {}

    public mutating func insert(_ string: String) {
        text += string
    }

    /// Mirror one `deleteBackward` — removes the last *grapheme* to stay aligned
    /// with `UITextDocumentProxy.deleteBackward()`.
    public mutating func deleteBackward() {
        if !text.isEmpty { text.removeLast() }
    }

    public mutating func reset() {
        text = ""
    }

    public var isEmpty: Bool { text.isEmpty }

    /// Grapheme count (what `deleteBackward` operates on).
    public var count: Int { text.count }
}
