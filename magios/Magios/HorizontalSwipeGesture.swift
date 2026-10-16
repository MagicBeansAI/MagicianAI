import SwiftUI
import UIKit

/// A horizontal swipe for cards that sit inside a vertical `ScrollView`.
///
/// Since iOS 18 a SwiftUI `DragGesture` on a scroll view's child — even as a
/// `simultaneousGesture` — keeps the scroll view's pan from starting, so a
/// vertical scroll that begins on a swipe card goes nowhere. A UIKit pan
/// recognizer can refuse to *begin* instead: it only starts when the finger
/// moves more sideways than up/down, so vertical drags fall straight through
/// to the page scroll and horizontal ones still drive the card. iOS 17 keeps
/// the SwiftUI gesture, where the simultaneous drag scrolls correctly.
@available(iOS 18.0, *)
private struct HorizontalPanRecognizer: UIGestureRecognizerRepresentable {
    let onChanged: (CGSize) -> Void
    let onEnded: (_ translation: CGSize, _ predicted: CGSize) -> Void
    let onCancelled: () -> Void

    /// How far a release's velocity projects the card, matching the Android
    /// deck's projection so a quick flick commits on both platforms alike.
    static let projectionSeconds: CGFloat = 0.2

    func makeUIGestureRecognizer(context: Context) -> UIPanGestureRecognizer {
        let pan = UIPanGestureRecognizer()
        pan.delegate = context.coordinator
        pan.maximumNumberOfTouches = 1
        return pan
    }

    func makeCoordinator(converter: CoordinateSpaceConverter) -> Coordinator { Coordinator() }

    func handleUIGestureRecognizerAction(_ recognizer: UIPanGestureRecognizer, context: Context) {
        let t = recognizer.translation(in: recognizer.view)
        let translation = CGSize(width: t.x, height: t.y)
        switch recognizer.state {
        case .began, .changed:
            onChanged(translation)
        case .ended:
            let v = recognizer.velocity(in: recognizer.view)
            let predicted = CGSize(
                width: t.x + v.x * Self.projectionSeconds,
                height: t.y + v.y * Self.projectionSeconds
            )
            onEnded(translation, predicted)
        case .cancelled, .failed:
            onCancelled()
        default:
            break
        }
    }

    final class Coordinator: NSObject, UIGestureRecognizerDelegate {
        /// Begin only for a sideways-dominant pan; a vertical one fails here
        /// and the enclosing scroll view takes the touch.
        func gestureRecognizerShouldBegin(_ gestureRecognizer: UIGestureRecognizer) -> Bool {
            guard let pan = gestureRecognizer as? UIPanGestureRecognizer else { return false }
            let velocity = pan.velocity(in: pan.view)
            return abs(velocity.x) > abs(velocity.y)
        }
    }
}

private struct HorizontalSwipeModifier<Fallback: Gesture>: ViewModifier {
    let fallback: Fallback
    let onChanged: (CGSize) -> Void
    let onEnded: (CGSize, CGSize) -> Void
    let onCancelled: () -> Void

    func body(content: Content) -> some View {
        if #available(iOS 18.0, *) {
            content.gesture(HorizontalPanRecognizer(onChanged: onChanged, onEnded: onEnded, onCancelled: onCancelled))
        } else {
            content.simultaneousGesture(fallback)
        }
    }
}

extension View {
    /// Attach a horizontal swipe that leaves vertical scrolling to the page.
    /// On iOS 18+ the callbacks drive the card; on iOS 17 `fallback` (the
    /// existing SwiftUI drag) is used unchanged.
    func horizontalSwipe<G: Gesture>(
        fallback: G,
        onChanged: @escaping (CGSize) -> Void,
        onEnded: @escaping (_ translation: CGSize, _ predicted: CGSize) -> Void,
        onCancelled: @escaping () -> Void
    ) -> some View {
        modifier(HorizontalSwipeModifier(fallback: fallback, onChanged: onChanged, onEnded: onEnded, onCancelled: onCancelled))
    }
}
