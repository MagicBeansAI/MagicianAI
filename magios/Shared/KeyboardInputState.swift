import Foundation

/// The keyboard's shift / caps-lock / layer state machine — pure value type so the
/// transitions are unit-tested without a running keyboard.
public struct KeyboardInputState: Equatable {
    public private(set) var layer: KeyboardLayer = .letters
    public private(set) var shift: ShiftState = .on   // sentence-start capital by default
    public init() {}

    /// Single shift tap toggles on/off (and clears caps-lock).
    public mutating func tapShift() {
        shift = (shift == .off) ? .on : .off
    }

    /// Double-tap engages caps-lock.
    public mutating func engageCapsLock() {
        shift = .capsLock
    }

    /// Max gap (seconds) between two Shift taps to count as a caps-lock double-tap.
    public static let shiftDoubleTapWindow: TimeInterval = 0.3

    /// Whether a Shift tap at `now` is the second of a caps-lock double-tap, given
    /// the previous tap's time. Pure (kept in Shared) so it's unit-testable without
    /// the view-layer model. The caller toggles immediately on a lone tap — there is
    /// NO gesture-disambiguation wait — and upgrades to caps-lock only when this
    /// returns true. (A `TapGesture(count: 2)` in the view would instead force every
    /// single tap to wait ~0.35–0.5s for the double-tap to fail — the Shift lag.)
    public static func shiftTapEngagesCapsLock(
        now: TimeInterval,
        lastTapAt: TimeInterval,
        window: TimeInterval = shiftDoubleTapWindow
    ) -> Bool {
        now - lastTapAt <= window
    }

    public mutating func switchLayer(_ target: KeyboardLayer) {
        layer = target
    }

    /// Called after inserting a character: a one-shot shift releases; caps-lock
    /// holds; number/symbol layers stay put (iOS parity — only ABC returns via the
    /// explicit key).
    public mutating func didTypeCharacter() {
        if shift == .on { shift = .off }
    }

    /// Recompute the sentence-start auto-capital from the text before the cursor.
    /// No-op under caps-lock or on non-letter layers.
    public mutating func refreshAutocapitalize(before context: String?) {
        guard layer == .letters, shift != .capsLock else { return }
        let ctx = context ?? ""
        if ctx.isEmpty {
            shift = .on
            return
        }
        // Capital after sentence-ending punctuation + a space (". ", "! ", "? ").
        let tail = String(ctx.suffix(2))
        if tail.count == 2,
           tail.last == " ",
           let prev = tail.first,
           ".!?".contains(prev) {
            shift = .on
        } else if ctx.last == " ", ctx.count == 1 {
            // leading space at field start
            shift = .on
        } else {
            shift = .off
        }
    }
}
