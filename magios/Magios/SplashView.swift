import SwiftUI

/// 3-second brand splash — mirrors the web root-page hero (default `magican` theme):
/// a slightly-slipped "Utterly Useless AI Agents" stamp, the Outfit "magican"
/// wordmark with an ink→coral→ink gradient, and the "I'll try my best." line.
/// Colors are the web `:root` palette (bg-base #fdfcf8, ink #2d3436,
/// secondary #5f6668, accent-primary #ff6b6b); fonts are the bundled brand
/// roles (Outfit brand/display, Manrope body, Geist Mono data) — see UIAppFonts.
struct SplashView: View {
    // Web :root (magican light) palette.
    private let cream = Color(hex: "fdfcf8")
    private let ink = Color(hex: "2d3436")
    private let secondary = Color(hex: "5f6668")
    private let coral = Color(hex: "ff6b6b")

    @State private var appeared = false

    var body: some View {
        ZStack {
            cream.ignoresSafeArea()

            VStack(spacing: 6) {
                // Badge — Outfit caps stamp, "slipped loose" (rotated, one side
                // lower) exactly like the web `.landing-badge` slip-loose end state.
                Text("Utterly Useless AI Agents")
                    .font(.custom("Outfit", size: 12).weight(.bold))
                    .textCase(.uppercase)
                    .kerning(1.0)
                    .foregroundColor(secondary)
                    .padding(.horizontal, 13)
                    .padding(.vertical, 6)
                    .background(
                        Capsule().fill(ink.opacity(0.03))
                            .overlay(Capsule().stroke(ink.opacity(0.08), lineWidth: 1))
                    )
                    .shadow(color: ink.opacity(0.08), radius: 0, x: 1, y: 1)
                    .rotationEffect(.degrees(-1.5))
                    .offset(y: 2)
                    .zIndex(1)

                // Wordmark — Outfit, ink→coral→ink gradient (matches the web title).
                Text("magican")
                    .font(.custom("Outfit", size: 128).weight(.regular))
                    .foregroundStyle(
                        LinearGradient(
                            colors: [ink, ink, coral, ink, ink],
                            startPoint: .leading,
                            endPoint: .trailing
                        )
                    )
                    .padding(.top, -6)

                // Subtitle — short italic line, paper-cream voice.
                Text("I'll try my best.")
                    .font(.custom("Manrope", size: 17))
                    .italic()
                    .foregroundColor(secondary)
                    .padding(.top, -10)
            }
            .scaleEffect(appeared ? 1 : 0.94)
            .opacity(appeared ? 1 : 0)

            // App version — pinned to the bottom, small + light.
            VStack {
                Spacer()
                Text("v\(AppInfo.version)")
                    .font(.custom("Geist Mono", size: 12))
                    .foregroundColor(secondary.opacity(0.55))
                    .padding(.bottom, 26)
            }
            .opacity(appeared ? 1 : 0)
        }
        .onAppear {
            withAnimation(.spring(response: 0.6, dampingFraction: 0.7)) { appeared = true }
        }
    }
}
