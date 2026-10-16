import SwiftUI

public extension Animation {
    /// How long a phase change takes to land, shared by every surface that
    /// renders an aurora orb. Lives here rather than in the widget so the
    /// in-app beacon and the task activity ease identically. Short on purpose:
    /// status indicators are glanced at, not watched.
    static let auroraPhaseEase = Animation.easeInOut(duration: 0.28)
}

/// The aurora orb: halo, gradient body, inner highlight, rim light. One view,
/// sized by diameter, so the lock screen, both islands, the task activity and
/// the in-app beacon cannot drift apart. At 22pt and up the body and rim are
/// an organic blob (`AuroraBlobShape`, seeded per phase by the palette), so
/// each phase owns its own still silhouette and the callers' identity-swap
/// cross-dissolve morphs shape along with colour; below 22pt the body stays a
/// circle — a blob is unreadable at dot sizes.
///
/// Out-of-process surfaces get NO app render loop, so the only motion here is
/// (1) the system's cross-dissolve of inserted/removed views — a same-identity
/// property change hard-cuts out there, so callers swap this view's IDENTITY
/// with the palette (`.id` on its gradient stops), and `auroraPhaseEase`
/// survives as the ≤2s timing hint iOS 17+ may honor on that swap — and (2)
/// the halo's repeating `.pulse`
/// symbol effect while `isActive` (live/under way: an ambient conversation, a
/// running task), which is system-rendered but device-confirmed **inert** in
/// Live Activities (iOS 26.5): out of process the halo is simply a static
/// glow, weaker but not false — the degradation this treatment always priced
/// in, and the reading those surfaces actually get. The effect stays declared
/// for in-process surfaces and for whatever a future OS renders out of
/// process. In-process callers may wrap this view in whatever live motion they
/// can honestly back.
public struct AuroraOrbView: View {
    let palette: AuroraPalette
    let diameter: CGFloat
    let isActive: Bool
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    public init(palette: AuroraPalette, diameter: CGFloat, isActive: Bool) {
        self.palette = palette
        self.diameter = diameter
        self.isActive = isActive
    }

    public var body: some View {
        ZStack {
            // Halo: an SF symbol rather than a Circle so `.pulse` can breathe
            // it — which it does in-process only; device verification found
            // the repeating effect inert on archived surfaces, where the halo
            // is the static glow the degradation promised. Reduce Motion
            // downgrades to a single non-repeating pass, the same idiom as the
            // indeterminate ellipsis.
            Image(systemName: "circle.fill")
                .font(.system(size: diameter * 1.35))
                .foregroundStyle(palette.halo)
                .opacity(palette.haloStrength)
                .blur(radius: diameter * 0.22)
                .symbolEffect(.pulse,
                              options: reduceMotion ? .nonRepeating : .repeating,
                              isActive: isActive)
            let bodyGradient = palette.bodyGradient
            let rimGradient = LinearGradient(colors: [palette.rim, palette.rim.opacity(0.05)],
                                             startPoint: .topLeading,
                                             endPoint: .bottomTrailing)
            // Ended keeps a perfect circle: `haloStrength == 0` uniquely
            // identifies graphite (the invariant test pins ended's halo at 0),
            // and amplitude 0 renders the blob as a circle — stillness and
            // geometry both say "over".
            let amplitude: Double = palette.haloStrength == 0 ? 0 : 0.10
            Group {
                if diameter >= 22 {
                    AuroraBlobShape(seed: palette.blobSeed, morph: 0, amplitude: amplitude)
                        .fill(bodyGradient)
                } else {
                    // A blob is unreadable at dot sizes (compact 11, minimal
                    // 10, task mini 14): below 22pt the body stays a circle
                    // and colour alone carries the phase, as it always has.
                    Circle()
                        .fill(bodyGradient)
                }
            }
            .frame(width: diameter, height: diameter)
            // Top-leading highlight: what makes it a sphere instead of a disc.
            // Stays a circle over the blob deliberately — it reads as light
            // falling on the body, not as the body's silhouette.
            Circle()
                .fill(RadialGradient(colors: [.white.opacity(0.55), .clear],
                                     center: UnitPoint(x: 0.32, y: 0.26),
                                     startRadius: 0,
                                     endRadius: diameter * 0.62))
                .frame(width: diameter, height: diameter)
            // The rim traces whichever silhouette the body drew — a stroked
            // circle over a blob body would read as a misregistered sticker.
            // Both shapes are insettable, so `strokeBorder` keeps the stroke
            // inside the fixed footprint either way.
            Group {
                if diameter >= 22 {
                    AuroraBlobShape(seed: palette.blobSeed, morph: 0, amplitude: amplitude)
                        .strokeBorder(rimGradient, lineWidth: max(1, diameter * 0.05))
                } else {
                    Circle()
                        .strokeBorder(rimGradient, lineWidth: max(1, diameter * 0.05))
                }
            }
            .frame(width: diameter, height: diameter)
        }
        // Fixed footprint: the halo overflows visually (it is a glow) but must
        // not push layout, or the compact island slot would jitter per phase.
        .frame(width: diameter, height: diameter)
    }
}

/// The hard cap drawn as a depleting ring around the orb. Time-driven —
/// `ProgressView(timerInterval:)` derives its own progress from the date range
/// — so it animates continuously for zero ActivityKit updates.
///
/// The full-size rings (lock screen, expanded island) sit beside a digit
/// `.timer`, which they never replace: the digits stay for precision, and they
/// are those sites' degradation if ring sizing misbehaves. The compact
/// conversing ring stands alone — no digits fit that slot — so there the ring
/// is glanceable presence only, and precision is one long-press away in the
/// expanded digits.
///
/// Determinate circular progress renders only on widget surfaces — in-process
/// iOS falls back to an indeterminate spinner — so this ring is for the Live
/// Activity presentations, and any in-app adoption needs its own device
/// verification first.
public struct AmbientLeashRing<Content: View>: View {
    let armedAt: Date
    let expiresAt: Date
    let tint: Color
    let diameter: CGFloat
    @ViewBuilder let center: () -> Content

    public init(armedAt: Date, expiresAt: Date, tint: Color, diameter: CGFloat,
                @ViewBuilder center: @escaping () -> Content) {
        self.armedAt = armedAt
        self.expiresAt = expiresAt
        self.tint = tint
        self.diameter = diameter
        self.center = center
    }

    public var body: some View {
        // `ClosedRange<Date>` traps on an inverted pair — the hazard
        // `AmbientSpeakingSpan` exists to absorb — so clamp: an inverted pair
        // degrades to an inert ring rather than crashing the widget process.
        let end = max(armedAt, expiresAt)
        return ZStack {
            ProgressView(timerInterval: armedAt...end, countsDown: true) {
                EmptyView()
            } currentValueLabel: {
                EmptyView()
            }
            .progressViewStyle(.circular)
            .tint(tint)
            .frame(width: diameter, height: diameter)
            // Hidden on the progress chain only: where digits accompany the
            // ring they are the readable copy of the cap, and in the compact
            // slot the glyph's status label carries the phase — either way the
            // gauge itself has nothing to add. Whatever sits in `center` keeps
            // its own accessibility voice.
            .accessibilityHidden(true)
            center()
        }
    }
}
