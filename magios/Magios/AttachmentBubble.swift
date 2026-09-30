import SwiftUI

struct AttachmentBubble: View {
    let filename: String
    let size: String?
    let isUser: Bool
    @ObservedObject var theme: ThemeManager
    
    var body: some View {
        HStack {
            if isUser { Spacer() }
            
            HStack(spacing: 12) {
                Image(systemName: "doc.text.fill")
                    .foregroundColor(isUser ? theme.onAccentColor : theme.accentColor)
                    .font(.title2)
                
                VStack(alignment: .leading, spacing: 2) {
                    Text(filename)
                        .font(.themed(15, weight: .bold))
                        .foregroundColor(isUser ? theme.onAccentColor : theme.textColor)
                        .lineLimit(1)
                    
                    if let size = size {
                        Text(size)
                            .font(.themed(11))
                            .foregroundColor(isUser ? theme.onAccentColor.opacity(0.8) : theme.secondaryTextColor)
                    }
                }
            }
            .padding(12)
            .background(isUser ? theme.accentColor : theme.cardColor)
            .cornerRadius(16)
            
            if !isUser { Spacer() }
        }
    }
}
