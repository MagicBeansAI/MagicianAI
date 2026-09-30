import SwiftUI

struct HealthPillView: View {
    @ObservedObject private var healthVM = HealthViewModel.shared
    @StateObject private var themeManager = ThemeManager.shared
    
    var body: some View {
        Button(action: { AppActions.shared.requestSettings() }) {
            HStack(spacing: 4) {
                Circle()
                    .fill(healthVM.magicianState.color)
                    .frame(width: 8, height: 8)
                Text("Magican")
                    .font(.themedBrand(11, weight: .bold))
                    .foregroundColor(themeManager.textColor)
            }
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Magican service status: \(healthVM.magicianState.label). Open Settings")
    }
}
