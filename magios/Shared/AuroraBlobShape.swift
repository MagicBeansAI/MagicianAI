import SwiftUI

/// A closed organic silhouette: a circle whose radius undulates around the
/// rim. Pure geometry over a seed — same seed, same blob, forever — because
/// out-of-process surfaces have no render loop: the blob cannot MOVE, so each
/// phase owns a distinct STILL silhouette and the identity-swap cross-dissolve
/// (the widget's mechanism 1) morphs between them on phase changes.
///
/// `morph` exists for the in-process caller: a continuously advancing phase
/// angle that slides the lobes around the rim. Out-of-process callers leave it
/// 0 and get the seed's still shape.
///
/// The rim is a three-frequency sine mix (2θ, 3θ, 5θ) rather than one wave:
/// a single frequency reads as a gear, coprime frequencies at falling weights
/// read as a living lobe. The weights sum to 1, so `amplitude` is exactly the
/// worst-case radial excursion, and the base radius is scaled down by
/// (1 + amplitude) so the tallest lobe still lands inside the rect — the
/// fixed-footprint contract every orb caller relies on.
public struct AuroraBlobShape: InsettableShape {
    /// Per-phase silhouette selector. Phase-shifts each frequency differently,
    /// so distinct seeds produce visibly distinct lobe arrangements.
    public var seed: Double
    /// In-process animation angle; 0 = still.
    public var morph: Double
    /// 0…1, lobe depth relative to the base radius. 0 renders a perfect circle.
    public var amplitude: Double
    /// `InsettableShape` so `strokeBorder` works exactly as it does on
    /// `Circle`: the rim stroke stays inside the footprint instead of
    /// overflowing it by half a line width.
    public var insetAmount: CGFloat = 0

    public init(seed: Double, morph: Double, amplitude: Double) {
        self.seed = seed
        self.morph = morph
        self.amplitude = amplitude
    }

    /// `morph` and `amplitude` interpolate; `seed` deliberately does not — a
    /// seed is an identity, not a position, and phase changes swap view
    /// identity rather than sliding one seed toward another. The amplitude's
    /// interpolation is real only under Reduce Motion, where the in-app
    /// beacon's timeline is paused and its mic-level transaction has frames to
    /// ease across; while the timeline drifts, every frame re-evaluates the
    /// body and per-frame values land as-is.
    public var animatableData: AnimatablePair<Double, Double> {
        get { AnimatablePair(morph, amplitude) }
        set {
            morph = newValue.first
            amplitude = newValue.second
        }
    }

    public func inset(by amount: CGFloat) -> AuroraBlobShape {
        var shape = self
        shape.insetAmount += amount
        return shape
    }

    public func path(in rect: CGRect) -> Path {
        let rect = rect.insetBy(dx: insetAmount, dy: insetAmount)
        guard rect.width > 0, rect.height > 0 else { return Path() }
        let amplitude = min(max(self.amplitude, 0), 1)
        // Peaks reach base * (1 + amplitude) == the half-extent: lobes touch
        // the rect edge, never cross it.
        let base = Double(min(rect.width, rect.height)) / 2 / (1 + amplitude)
        let center = CGPoint(x: rect.midX, y: rect.midY)
        // 180 samples with straight segments: at orb sizes each segment spans
        // well under a point, so a spline would smooth nothing visible.
        let samples = 180
        var path = Path()
        for index in 0..<samples {
            let theta = Double(index) / Double(samples) * 2 * .pi
            let wave = 0.55 * sin(2 * theta + seed)
                + 0.30 * sin(3 * theta + seed * 1.7 + morph)
                + 0.15 * sin(5 * theta - seed * 2.3 + morph * 1.4)
            // The weights already bound `wave` to [-1, 1]; the clamp makes the
            // radius envelope a guarantee rather than an arithmetic accident.
            let radius = base * (1 + amplitude * min(max(wave, -1), 1))
            let point = CGPoint(x: center.x + CGFloat(cos(theta) * radius),
                                y: center.y + CGFloat(sin(theta) * radius))
            if index == 0 {
                path.move(to: point)
            } else {
                path.addLine(to: point)
            }
        }
        path.closeSubpath()
        return path
    }
}
